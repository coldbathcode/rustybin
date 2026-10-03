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
| ANY | `/status/{code}` | Respond with any HTTP status code (200-599), or a weighted random choice |

### Response Shaping

| Methods | Path | Description |
|---|---|---|
| ANY | `/delay/{ms}` | Wait, then respond: milliseconds (1500), `250ms` or seconds (`1.5s`); ?jitter=true adds +-20% |
| GET | `/cache/{ttl}` | Cache-Control, ETag and Last-Modified; 304 on If-None-Match / If-Modified-Since |
| GET | `/response-headers` | Query parameters become response headers (hop-by-hop / framing headers refused) |

### Redirects & Cookies

| Methods | Path | Description |
|---|---|---|
| GET | `/redirect/{n}` | Chain of n relative 302 redirects |
| GET | `/cookies` | Request cookies as JSON |
| GET | `/cookies/set` | Set cookies from query parameters, then 302 to /cookies |
| GET | `/cookies/set/{name}/{value}` | Set a single cookie (Path=/) |
| GET | `/cookies/delete` | Expire the cookies named in the query string (_path / _domain must match how they were set) |

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
| POST | `/ai/v1/chat/completions` | Chat completions (SSE streaming with stream=true) |
| POST | `/ai/v1/completions` | Legacy text completions |
| POST | `/ai/v1/embeddings` | Deterministic 1536-dimension embeddings |
| GET | `/ai/v1/models` | List available models |

### AI: Anthropic-compatible

| Methods | Path | Description |
|---|---|---|
| POST | `/ai/anthropic/v1/messages` | Messages API (native SSE event stream with stream=true) |

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

### Health & Identity

| Methods | Path | Description |
|---|---|---|
| GET | `/health` | Health check (200, or 503 when toggled unhealthy) |
| POST | `/health/healthy` | Mark the instance healthy (admin-guarded) |
| POST | `/health/unhealthy` | Mark the instance unhealthy: 200 here, then /health returns 503 (admin-guarded) |
| POST | `/health/toggle` | Flip the health state, 200 with the new state (admin-guarded) |
| ANY | `/identity` | Instance identity: id, hostname, uptime, request count, ports and config (load-balancing demos) |

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
| `X-Rustybin-Fail: <status>` or `<status>:<percent>` | Inject an error response (status 400-599), always or with the given probability, e.g. `503:50` |
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
| AI proxying | `/ai/v1/chat/completions`, `/ai/anthropic/v1/messages` | OpenAI- and Anthropic-format upstreams |
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
