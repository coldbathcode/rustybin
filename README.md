# Rustybin

A high-performance, all-in-one HTTP stub/echo service written in Rust using [axum](https://github.com/tokio-rs/axum). Rustybin is designed to exercise every category of API gateway capability - auth, routing, transformation, rate limiting, AI proxying, gRPC, WebSocket, and more - from a single binary. Think httpbin, but faster, broader, and purpose-built for gateway demos and testing.

## Quick Start

```bash
docker compose up -d
# HTTP on :80, HTTPS on :443
curl http://localhost/health
```

Or with Docker directly:

```bash
docker build -t rustybin .
docker run -d -p 80:80 -p 443:443 --name rustybin rustybin
```

## Endpoints

The tables below are generated from the route catalogue (`src/catalog.rs`), the same
source as the landing page (`GET /`), the OpenAPI spec (`/openapi.json`) and the
collection exports (`/export/*`). A test fails when they drift apart.

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
| ANY | `/status/{code}` | Respond with any HTTP status code (200-599) |

### Response Shaping

| Methods | Path | Description |
|---|---|---|
| ANY | `/delay/{ms}` | Wait ms milliseconds, then respond (?jitter=true adds variance) |
| GET | `/cache/{ttl}` | Cache-Control and ETag headers, 304 on If-None-Match |
| GET | `/response-headers` | Query parameters become response headers |

### Redirects & Cookies

| Methods | Path | Description |
|---|---|---|
| GET | `/redirect/{n}` | Chain of n relative 302 redirects |
| GET | `/cookies` | Request cookies as JSON |
| GET | `/cookies/set` | Set cookies from query parameters |
| GET | `/cookies/set/{name}/{value}` | Set a single cookie |
| GET | `/cookies/delete` | Delete the cookies named in the query string |

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
| GET | `/image/png` | Minimal PNG image |
| GET | `/image/jpeg` | Minimal JPEG image |
| GET | `/image/gif` | Minimal GIF image |

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
| ANY | `/auth/hmac` | Validate an hmac-auth style signature (default alice / secret) |
| ANY | `/auth/hmac/{username}/{secret}` | HMAC validation with username and secret from the path |

### Auth: JWT

| Methods | Path | Description |
|---|---|---|
| ANY | `/auth/jwt` | Decode and validate a Bearer JWT (structure, no signature check) |
| ANY | `/auth/jwt/exchange` | Exchange a JWT for a new HS256-signed token |

### Auth: OIDC Provider

| Methods | Path | Description |
|---|---|---|
| GET | `/.well-known/openid-configuration` | OIDC discovery document |
| POST | `/oauth/token` | Token endpoint (client_credentials, password, authorization_code, refresh, token exchange) |
| GET | `/oauth/jwks` | RS256 public key in JWK Set format |
| GET POST | `/oauth/authorize` | Authorization code flow with a demo login form |
| GET | `/oauth/userinfo` | User claims for a Bearer access token |
| POST | `/oauth/introspect` | Token introspection (RFC 7662) |

### Auth: mTLS

| Methods | Path | Description |
|---|---|---|
| ANY | `/auth/mtls` | Validate the client certificate (TLS or forwarded header) |
| GET | `/auth/mtls/get-client-cert` | Download the demo client certificate and key |
| GET | `/auth/mtls/get-ca-cert` | Download the demo CA certificate |

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

### GraphQL

| Methods | Path | Description |
|---|---|---|
| GET POST | `/graphql` | GraphQL endpoint (GET: playground, POST: query) |
| GET | `/graphql/schema` | Schema in SDL |

### Orchestration

| Methods | Path | Description |
|---|---|---|
| POST | `/orchestration/step/1` | Step 1: authenticate (X-Api-Key required) |
| POST | `/orchestration/step/2` | Step 2: enrich (X-Correlation-Id required) |
| POST | `/orchestration/step/3` | Step 3: validate (risk scoring) |
| POST | `/orchestration/step/4` | Step 4: process (requires X-Validation-Result: approved) |
| GET | `/orchestration/status` | Pipeline documentation |

### SOAP / XML

| Methods | Path | Description |
|---|---|---|
| POST | `/soap` | SOAP 1.1 service (GetUser, ListUsers, CreateUser) |
| GET | `/soap/wsdl` | WSDL document |

### WebSocket

| Methods | Path | Description |
|---|---|---|
| GET | `/ws` | WebSocket echo of every text and binary frame (WebSocket) |
| GET | `/ws/time` | WebSocket timestamp ticker (?interval_ms=&count=) (WebSocket) |

### Reliability Testing

| Methods | Path | Description |
|---|---|---|
| ANY | `/flaky/{fail_rate}` | Fail with 503 for fail_rate percent of requests |
| ANY | `/flaky/pattern/{pattern}` | Deterministic success/failure pattern (S = success, F = failure) |
| ANY | `/flaky/after/{n}` | Succeed n times, then fail (circuit breaker trip) |
| ANY | `/flaky/recover/{n}` | Fail n times, then recover (circuit breaker half-open) |
| POST | `/flaky/reset` | Reset all flaky counters (admin-guarded) |
| GET | `/flaky/status` | Current flaky counters |

### Health & Identity

| Methods | Path | Description |
|---|---|---|
| GET | `/health` | Health check (200, or 503 when toggled unhealthy) |
| POST | `/health/healthy` | Mark the instance healthy (admin-guarded) |
| POST | `/health/unhealthy` | Mark the instance unhealthy, /health returns 503 (admin-guarded) |
| POST | `/health/toggle` | Flip the health state (admin-guarded) |
| ANY | `/identity` | Instance identity: id, hostname, uptime, request count (load-balancing demos) |

### Control Plane

| Methods | Path | Description |
|---|---|---|
| GET DELETE | `/_rustybin/requests` | List captured requests (newest first); DELETE clears them |
| GET | `/_rustybin/requests/stream` | Live feed of captured requests (SSE) |
| GET | `/_rustybin/requests/{id}` | One captured request by id |
| GET | `/_rustybin/config` | Effective configuration (no secrets) |
| GET | `/_rustybin/version` | Service name and version |

### Docs & Exports

| Methods | Path | Description |
|---|---|---|
| GET | `/` | This landing page (always 200, safe for liveness checks) |
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

In addition, a gRPC `EchoService` (unary, server, client and bidi streaming) listens on
its own port (default `50051`, see [gRPC](#grpc)).

All JSON endpoints support content negotiation: send `Accept: application/xml` for XML responses.

## Cross-cutting features

These work on every route except the control plane (`/_rustybin/*`) and the console (`/ui/*`):

| Header | Effect |
|---|---|
| `X-Rustybin-Delay: <ms>` | Delay the response (capped at 30 s, 10 s in public mode) |
| `X-Rustybin-Fail: <status>` or `<status>:<percent>` | Inject an error response (status 400-599), always or with the given probability, e.g. `503:50` (on `/ai/*` the provider's native error, see [Mock LLM](#mock-llm)) |
| `X-Request-Id` | Propagated when sent, generated otherwise, and echoed on the response |
| `X-Rustybin-Session: <id>` | Tags the request for the inspector (`/_rustybin/requests?session=<id>`) and scopes per-client state |

**Request inspector**: recent requests (method, URI, headers, client IP, body up to 64 KB,
status, latency) are kept in a bounded ring buffer (`RUSTYBIN_INSPECTOR_CAPACITY`).
List them with `GET /_rustybin/requests`, fetch one with `GET /_rustybin/requests/{id}`,
or follow them live with `GET /_rustybin/requests/stream` (SSE).

**Admin token**: when `RUSTYBIN_ADMIN_TOKEN` is set, instance-global mutations
(`/health/healthy|unhealthy|toggle`, `/flaky/reset`, clearing all captured requests)
require `Authorization: Bearer <token>` or `X-Rustybin-Admin-Token: <token>`.

**Public mode** (`RUSTYBIN_PUBLIC_MODE=true`) is meant for shared, internet-facing
instances: only requests carrying `X-Rustybin-Session` are captured, the inspector only
returns entries for the `?session=` you ask for, delays are capped lower, and global
mutations are disabled unless an admin token is configured.

## Mock LLM

A deterministic multi-provider model server for AI gateway demos (AI proxy,
token rate limiting, prompt guard, prompt decorators/templates, semantic
cache, PII sanitisers, load balancing/failover, credential injection,
request/response transformers). The official SDKs accept it
(`conformance/ai/` runs the OpenAI, Anthropic and Google Gen AI SDKs).

| Provider | Point the client at | Routes |
|---|---|---|
| OpenAI | `base_url=http://HOST/ai/openai/v1` (or the legacy `/ai/v1`) | `chat/completions`, `completions`, `embeddings`, `models`, `models/{model}`, `moderations`, `responses`, `images/generations`, `audio/transcriptions` |
| Azure OpenAI | `azure_endpoint=http://HOST/ai/azure` | `openai/deployments/{deployment}/chat/completions`, `/completions`, `/embeddings` (`?api-version=` required) |
| Anthropic | `base_url=http://HOST/ai/anthropic` | `v1/messages`, `v1/messages/count_tokens`, `v1/models` |
| Gemini | `http_options.base_url=http://HOST/ai/gemini` | `v1beta/models/{model}:generateContent`, `:streamGenerateContent` (`?alt=sse`), `:countTokens`, `:embedContent`, `:batchEmbedContents`, `v1beta/models` |
| AWS Bedrock | endpoint `http://HOST/ai/bedrock` | `model/{modelId}/converse`, `/converse-stream` (binary event stream), `/invoke`, `/invoke-with-response-stream` |
| Ollama | `http://HOST/ai/ollama` | `api/chat`, `api/generate` (NDJSON, streaming by default), `api/tags`, `api/embed` |
| Cohere | `http://HOST/ai/cohere` | `v2/rerank`, `v2/embed` (plus the generic `/ai/v1/rerank`) |

**Modes** (header `X-Rustybin-Mode`, or a model name segment split on `- _ : / . @`,
e.g. `rustybin-echo`, `gpt-4o:scripted`, `random`):

| Mode | Reply |
|---|---|
| `canned` (default) | Keyword table with whole-word matching: greetings (`hi`, `hello`; "this" does not match), code, JSON/data, long/essay, else a default text |
| `echo` | The exact prompt the upstream received (`system: ...`, `user: ...`, tool calls and results), so prompt decorator/template plugins can be shown |
| `scripted` | Demo rules on the last user message, below |
| `random` | Text seeded by a SHA-256 hash of model + prompt (identical requests get identical text, `n>1` choices differ) |

| Scripted trigger (whole words) | Reply |
|---|---|
| `lorem N` | N lorem ipsum words (max 4000, 500 in public mode) |
| `ssn`, `social security`, `credit card`, `pii`, `customer record` | Fake PII: SSN `123-45-6789`, card `4111 1111 1111 1111`, email, phone (response sanitiser demos) |
| `toxic`, `jailbreak`, `unsafe`, `harmful` | A simulated unsafe answer an output guardrail should block |
| `secret`, `api key`, `password`, `credentials` | Fake cloud keys and tokens (secret redaction demos) |
| `refuse`, `refusal` | A refusal |
| `json` | A bare JSON object |
| `markdown`, `table` | Markdown with a table and links |
| `url`, `link` | Text with allowed and suspicious URLs |
| `echo` | The rendered prompt |

**Tools and structured output**: when tools are supplied and the tool choice is not
`none`, a tool call is returned if the tool is forced (`required` / `any` / a named tool)
or the last user message mentions the tool name or a description keyword (whole words).
Arguments are generated from the tool's JSON schema (required and optional properties,
enums, nested objects/arrays, `$ref` into `$defs`, formats, plausible strings by property
name, e.g. `location` takes "Paris" from "weather in Paris"). After a tool result, the
answer quotes the tool output. `response_format` json_schema / json_object (OpenAI),
`text.format` (Responses), `responseSchema` (Gemini), `output_format` or a forced tool
(Anthropic) and `format` (Ollama) produce JSON valid against the schema.

**Tokens**: words cost `ceil(chars / 4)` tokens, every punctuation mark or symbol 1,
images 85; prompt tokens count the rendered prompt plus tool definitions. The same count
drives usage fields, rate-limit headers, `max_tokens` / `max_output_tokens` truncation
(finish reason `length` / `max_tokens` / `MAX_TOKENS` / `incomplete`) and stream chunks
(one token per delta). Anthropic `cache_control` prefixes report
`cache_creation_input_tokens` on the first request and `cache_read_input_tokens` on
repeats within 5 minutes.

**Embeddings** are a hashed bag of words, word pairs and character trigrams (FNV-1a),
normalised: identical input gives identical vectors, case and punctuation are ignored,
reordered words stay close (cosine about 0.9), unrelated text is near 0. Rerank scores
blend query-word overlap with that similarity.

**Request headers** (values capped; latency caps follow `X-Rustybin-Delay`):

| Header | Effect |
|---|---|
| `X-Rustybin-Mode` | `canned`, `echo`, `scripted`, `random` |
| `X-Rustybin-Latency-Ms` | Delay before the response headers |
| `X-Rustybin-TTFT-Ms` | Time to first token (streams: after the headers; otherwise added to latency) |
| `X-Rustybin-Tokens-Per-Second` | Streaming pace (default 100, `0` = unpaced; a stream's pacing is capped at 60 s, 20 s in public mode) |
| `X-Rustybin-Fail` | `kind[:percent]` in the provider's native error shape: `429`/`rate_limit` (with `retry-after` and provider rate-limit headers), `500`, `503`, `529`/`overloaded` (Anthropic 529, others 503), `504`/`timeout`, `context_length` (400 `context_length_exceeded`), `content_filter` (200 with the filtered finish reason), `prompt_filter` (400), `401`, `403`, `404`, any 400-599. Also `?fail=kind&fail_rate=0.3` |
| `X-Rustybin-Require-Auth: true` | Enforce the provider's native credential (below) |

**Credentials** (enforced with `X-Rustybin-Require-Auth`, `RUSTYBIN_AI_REQUIRE_AUTH=true`,
or `RUSTYBIN_AI_API_KEY=<key>`, which also requires that exact key): OpenAI, Ollama and
Cohere `Authorization: Bearer`; Azure `api-key`; Anthropic `x-api-key` plus
`anthropic-version`; Gemini `x-goog-api-key` or `?key=`; Bedrock a SigV4
`Authorization: AWS4-HMAC-SHA256 Credential=.../SignedHeaders=...,Signature=...` header
plus `X-Amz-Date` (structure only) or a Bedrock API key bearer. Failures use the native
status and shape (401, Gemini/Bedrock 403).

**Response headers** on every `/ai/*` call: `X-Rustybin-Request-Id` (the `X-Request-Id`
when sent), `X-Rustybin-Credential` (the credential seen, redacted to its last four
characters, or `none`, to prove a gateway injected it), `X-Rustybin-Provider`,
`X-Rustybin-Instance`, `X-Rustybin-Model`, `X-Rustybin-Mode`, and realistic rate-limit
headers (`x-ratelimit-*` for OpenAI/Azure, `anthropic-ratelimit-*` for Anthropic).

**Inspection**: `GET /ai/requests/{id}` (id from `X-Rustybin-Request-Id`) shows what the
upstream received: headers (credentials redacted), body, normalised prompt, provider,
model, mode and token counts; `GET /ai/requests` lists recent exchanges. Records are
bounded (1000, 200 in public mode) and expire after an hour; in public mode callers only
see their own session (`X-Rustybin-Session` or client IP).

## Configuration

| Environment Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_HTTP_PORT` | `80` | HTTP listen port (failing to bind it is fatal) |
| `RUSTYBIN_HTTPS_PORT` | `443` | HTTPS listen port (optional: a failure only logs a warning) |
| `RUSTYBIN_GRPC_PORT` | `50051` | gRPC (EchoService) listen port (optional) |
| `RUSTYBIN_HOST` | `0.0.0.0` | Bind address, IPv4 or IPv6 (e.g. `::`) |
| `RUSTYBIN_LOG_LEVEL` | `info` | Tracing filter (`debug`, `info`, `warn`, `error`, or a full `EnvFilter`); falls back to `RUST_LOG` |
| `RUSTYBIN_TRUST_FORWARD` | `false` | Trust `X-Forwarded-For` / `X-Forwarded-Proto` for client IP and scheme detection |
| `RUSTYBIN_BODY_LIMIT` | `1048576` | Maximum request body in bytes; larger requests get `413` (also the echo display limit) |
| `RUSTYBIN_INSTANCE_ID` | Random UUID | Instance identifier for load balancer demos |
| `RUSTYBIN_TLS_CERT` | `certs/server.crt` | TLS certificate path (a demo certificate is generated when missing) |
| `RUSTYBIN_TLS_KEY` | `certs/server.key` | TLS private key path |
| `RUSTYBIN_MTLS_IN_HEADER` | _(unset)_ | Header name containing URL-encoded client cert PEM (for mTLS behind an L4 proxy) |
| `RUSTYBIN_PUBLIC_MODE` | `false` | Shared/public instance hardening (see [Public mode](#cross-cutting-features)) |
| `RUSTYBIN_ADMIN_TOKEN` | _(unset)_ | Token required for instance-global mutations; never exposed by any endpoint |
| `RUSTYBIN_CORS_ORIGINS` | `*` | Comma-separated allowed CORS origins; `off` disables CORS handling so a gateway's own CORS can be demonstrated |
| `RUSTYBIN_REQUEST_TIMEOUT` | `120` | Seconds until response headers must be ready (`503` otherwise); streams (SSE, WebSocket) are not cut off; `0` disables |
| `RUSTYBIN_INSPECTOR_CAPACITY` | `500` | Number of requests kept by the inspector (max 10000) |
| `RUSTYBIN_AI_REQUIRE_AUTH` | `false` | Mock LLM: enforce each provider's native credential (see [Mock LLM](#mock-llm)) |
| `RUSTYBIN_AI_API_KEY` | _(unset)_ | Mock LLM: the only accepted key (implies `RUSTYBIN_AI_REQUIRE_AUTH`; compared with the SigV4 access key id on Bedrock) |

Invalid values are logged as warnings and the default is used. `GET /_rustybin/config`
shows the effective (non-secret) configuration.

## Running Multiple Instances

For load balancer and health check demos, run multiple instances with distinct IDs:

```bash
docker run -d -p 8001:80 -e RUSTYBIN_INSTANCE_ID=instance-01 --name rb1 rustybin
docker run -d -p 8002:80 -e RUSTYBIN_INSTANCE_ID=instance-02 --name rb2 rustybin
docker run -d -p 8003:80 -e RUSTYBIN_INSTANCE_ID=instance-03 --name rb3 rustybin
```

Each instance returns its ID via `/identity`, making it easy to see which upstream the gateway or load balancer selected.

## Building from Source

```bash
# Debug build
cargo build

# Release build (optimised)
cargo build --release

# Run tests
cargo test

# Lint
cargo fmt --check
cargo clippy --all-targets -- -D warnings

# Run directly
RUSTYBIN_HTTP_PORT=8080 cargo run

# Print the endpoint tables of this README from the route catalogue
cargo run -- --print-endpoints-markdown
```

Requires Rust 1.86+.

## API Gateway Integration

Point any API gateway (or load balancer) at Rustybin as the upstream - `http://rustybin:80` for HTTP/HTTPS and `:50051` for gRPC - then route traffic through the gateway to exercise its policies.

### Examples (assuming the gateway listens on `:8000`)

**Rate limiting** - hit `/echo` and inspect the rate-limit headers the gateway adds:
```bash
curl -i http://gateway:8000/echo
```

**Auth** - validate credentials forwarded by the gateway:
```bash
curl -u alice:secret http://gateway:8000/auth/basic-auth/alice/secret
```

**JWT / OIDC** - get a token from the built-in provider, then validate it through the gateway:
```bash
TOKEN=$(curl -s -X POST http://rustybin/oauth/token \
  -d 'grant_type=client_credentials&client_id=rustybin&client_secret=secret' \
  | jq -r .access_token)
curl -H "Authorization: Bearer $TOKEN" http://gateway:8000/auth/jwt
```

**Retry / circuit breaking** - use `/flaky/50` to return 503 for half of requests:
```bash
for i in $(seq 1 20); do curl -s -o /dev/null -w "%{http_code}\n" http://gateway:8000/flaky/50; done
```

**Transformation** - inspect headers/body the gateway adds or rewrites via `/echo`:
```bash
curl -s http://gateway:8000/echo | jq .headers
```

## Capability Testing Matrix

A vendor-neutral map of gateway capabilities to the endpoints that exercise them:

| Gateway capability | Rustybin endpoint | What to test |
|---|---|---|
| Rate limiting | `/echo`, `/anything` | Rate-limit headers in echo response |
| Basic auth | `/auth/basic-auth/{u}/{p}` | Credential forwarding |
| API-key auth | `/auth/api-key/{header}/{key}` | API key forwarding |
| JWT auth | `/auth/jwt`, `/oauth/jwks` | JWT validation, JWKS endpoint |
| HMAC auth | `/auth/hmac`, `/auth/hmac/{u}/{s}` | HMAC signature validation |
| OAuth2 / OIDC | `/.well-known/openid-configuration` | Full OIDC flow |
| mTLS | `/auth/mtls` | Client certificate validation |
| Request termination | `/status/{code}`, `X-Rustybin-Fail` | Custom error responses |
| Proxy caching | `/cache/{ttl}` | Cache headers, conditional requests |
| Response transformation | `/echo` | Header/body inspection |
| Retry | `/flaky/{rate}`, `/flaky/pattern/{p}` | Retry on 503 |
| Circuit breaking | `/flaky/after/{n}`, `/flaky/recover/{n}` | Health state transitions |
| AI proxying | `/ai/openai/v1/*`, `/ai/anthropic/v1/*`, `/ai/gemini/v1beta/*`, `/ai/bedrock/*`, `/ai/azure/*`, `/ai/ollama/*` | Multi-provider mock LLM (see [Mock LLM](#mock-llm)) |
| AI rate limiting / failover | `X-Rustybin-Fail: 429`, `X-Rustybin-Latency-Ms`, usage fields | Native 429/529 errors, token usage, rate-limit headers |
| Prompt guard / decorator | `rustybin-echo`, `rustybin-scripted` models | Echo the decorated prompt, unsafe and PII replies |
| Semantic cache | `/ai/openai/v1/embeddings` | Deterministic, similarity-preserving embeddings |
| gRPC proxying | `EchoService` on `:50051` | gRPC unary + streaming proxying |
| WebSocket proxying | `/ws`, `/ws/time` | WebSocket frame proxying |
| Active health checks | `/health/unhealthy`, `/health/healthy` | Health-check failover |
| GraphQL proxying | `/graphql` | GraphQL query proxying |
| SOAP / XML | `/soap`, `/soap/wsdl` | SOAP envelope routing |
| Multi-step orchestration | `/orchestration/step/1-4` | Chained request pipeline |
| Redirect following | `/redirect/{n}` | Relative redirect chains |
| CORS | `/echo` | CORS header inspection |
| Request size limiting | `/echo` | Body size in echo response (Rustybin itself returns 413 above `RUSTYBIN_BODY_LIMIT`) |
| IP restriction | `/ip` | Client IP detection |

## gRPC

Rustybin serves a gRPC `EchoService` on a separate port (default `50051`,
`RUSTYBIN_GRPC_PORT`) for testing API gateway gRPC proxying (unary, streaming,
and web/HTTP transcoding). It implements all four call types - unary, server
streaming, client streaming, and bidirectional streaming - echoing the request
message along with the reflected request metadata and the handling instance ID.

The service definition lives in [`proto/echo.proto`](proto/echo.proto). Test it
with [`grpcurl`](https://github.com/fullstorydev/grpcurl):

```bash
# Unary
grpcurl -plaintext -d '{"message":"ping"}' \
  localhost:50051 rustybin.echo.v1.EchoService/Echo

# Server streaming - emit 5 responses
grpcurl -plaintext -d '{"message":"tick","count":5}' \
  localhost:50051 rustybin.echo.v1.EchoService/ServerStream
```

## WebSocket

```bash
# Echo (using websocat or wscat)
websocat ws://localhost/ws

# Server-push ticker: 5 timestamps, one every 500ms
websocat "ws://localhost/ws/time?interval_ms=500&count=5"
```

## API Documentation

Visit `/docs` for interactive API documentation powered by [Scalar](https://github.com/scalar/scalar), or fetch the raw spec:

```bash
curl http://localhost/openapi.json  # OpenAPI 3.0.3 JSON
curl http://localhost/openapi.yaml  # YAML format
```
