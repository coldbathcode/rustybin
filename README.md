# Rustybin

A high-performance, all-in-one HTTP stub/echo service written in Rust using [axum](https://github.com/tokio-rs/axum). Rustybin is designed to exercise every category of API gateway capability — auth, routing, transformation, rate limiting, AI proxying, gRPC, WebSocket, and more — from a single binary. Think httpbin, but faster, broader, and purpose-built for gateway demos and testing.

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

| Category | Endpoints | Description |
|---|---|---|
| **Health & Identity** | `/health`, `/health/healthy`, `/health/unhealthy`, `/health/toggle`, `/identity` | Service health check (runtime-toggleable to 503 for active health-check demos) and instance identity |
| **Echo** | `/echo`, `/anything` | Echo back full request details (method, headers, body, query params) |
| **Status Codes** | `/status/{code}` | Return any HTTP status code (100-599) |
| **Response Shaping** | `/delay/{ms}`, `/bytes/{n}`, `/stream/{n}`, `/drip`, `/cache/{ttl}`, `/response-headers` | Control response timing, size, streaming, caching |
| **Redirects** | `/redirect/{n}`, `/absolute-redirect/{n}`, `/redirect-to` | Redirect chains (relative and absolute) |
| **Cookies** | `/cookies`, `/cookies/set`, `/cookies/delete` | Cookie inspection and management |
| **Info** | `/ip`, `/date`, `/time` | Client IP, current date/time with timezone support |
| **Random** | `/uuid`, `/guuid`, `/random/*`, `/random/lorem-ipsum` | Random data generation (UUIDs, integers, text) |
| **Images** | `/image/png`, `/image/jpeg`, `/image/gif` | Static test images |
| **Basic Auth** | `/auth/basic-auth`, `/auth/basic-auth/{user}/{pass}` | HTTP Basic authentication with default or custom credentials |
| **API Key Auth** | `/auth/api-key`, `/auth/api-key/{header}/{key}` | API key header authentication |
| **JWT Auth** | `/auth/jwt`, `/auth/jwt/exchange` | JWT validation and token exchange (HS256) |
| **HMAC Auth** | `/auth/hmac`, `/auth/hmac/{user}/{secret}` | Gateway hmac-auth style signature validation (sha1/256/384/512) |
| **OIDC Provider** | `/.well-known/openid-configuration`, `/oauth/token`, `/oauth/jwks`, `/oauth/authorize`, `/oauth/userinfo`, `/oauth/introspect` | Full OpenID Connect Identity Provider |
| **mTLS** | `/auth/mtls`, `/auth/mtls/get-client-cert`, `/auth/mtls/get-ca-cert` | Mutual TLS with demo PKI |
| **AI Gateway (OpenAI)** | `/ai/v1/chat/completions`, `/ai/v1/completions`, `/ai/v1/embeddings`, `/ai/v1/models` | OpenAI-compatible endpoints for AI gateway testing |
| **AI Gateway (Anthropic)** | `/ai/anthropic/v1/messages` | Anthropic Messages API shape with native SSE event streaming |
| **GraphQL** | `/graphql`, `/graphql/schema` | GraphQL API with playground (users, products, orders) |
| **WebSocket** | `/ws`, `/ws/time` | Frame echo and server-push ticker for WebSocket proxying |
| **gRPC** | `EchoService` on `:50051` | Unary + server/client/bidi streaming echo (separate port) |
| **Orchestration** | `/orchestration/step/1-4`, `/orchestration/status` | Multi-step payment processing pipeline |
| **SOAP** | `/soap`, `/soap/wsdl` | SOAP/XML web service with WSDL |
| **Flaky** | `/flaky/{rate}`, `/flaky/pattern/{p}`, `/flaky/after/{n}`, `/flaky/recover/{n}` | Configurable failure simulation for circuit breaker testing |
| **Docs** | `/openapi.json`, `/openapi.yaml`, `/docs` | OpenAPI 3.0.3 spec and interactive Scalar UI |

All JSON endpoints support content negotiation — send `Accept: application/xml` for XML responses.

## Configuration

| Environment Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_HTTP_PORT` | `80` | HTTP listen port |
| `RUSTYBIN_HTTPS_PORT` | `443` | HTTPS listen port |
| `RUSTYBIN_GRPC_PORT` | `50051` | gRPC (EchoService) listen port |
| `RUSTYBIN_HOST` | `0.0.0.0` | Bind address |
| `RUSTYBIN_LOG_LEVEL` | `info` | Tracing log level (`debug`, `info`, `warn`, `error`) |
| `RUSTYBIN_TRUST_FORWARD` | `false` | Trust `X-Forwarded-*` headers for IP/scheme detection |
| `RUSTYBIN_BODY_LIMIT` | `1048576` | Max request body size in bytes (1MB) |
| `RUSTYBIN_INSTANCE_ID` | Random UUID | Instance identifier for load balancer demos |
| `RUSTYBIN_TLS_CERT` | `certs/server.crt` | Path to TLS certificate |
| `RUSTYBIN_TLS_KEY` | `certs/server.key` | Path to TLS private key |
| `RUSTYBIN_MTLS_IN_HEADER` | _(unset)_ | Header name containing URL-encoded client cert PEM (for mTLS behind L4 proxy) |

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
cargo clippy -- -D warnings

# Run directly
RUSTYBIN_HTTP_PORT=8080 cargo run
```

Requires Rust 1.82+.

## API Gateway Integration

Point any API gateway (or load balancer) at Rustybin as the upstream — `http://rustybin:80` for HTTP/HTTPS and `:50051` for gRPC — then route traffic through the gateway to exercise its policies.

### Examples (assuming the gateway listens on `:8000`)

**Rate limiting** — hit `/echo` and inspect the rate-limit headers the gateway adds:
```bash
curl -i http://gateway:8000/echo
```

**Auth** — validate credentials forwarded by the gateway:
```bash
curl -u alice:secret http://gateway:8000/auth/basic-auth/alice/secret
```

**JWT / OIDC** — get a token from the built-in provider, then validate it through the gateway:
```bash
TOKEN=$(curl -s -X POST http://rustybin/oauth/token \
  -d 'grant_type=client_credentials&client_id=rustybin&client_secret=secret' \
  | jq -r .access_token)
curl -H "Authorization: Bearer $TOKEN" http://gateway:8000/auth/jwt
```

**Retry / circuit breaking** — use `/flaky/50` to return 503 for half of requests:
```bash
for i in $(seq 1 20); do curl -s -o /dev/null -w "%{http_code}\n" http://gateway:8000/flaky/50; done
```

**Transformation** — inspect headers/body the gateway adds or rewrites via `/echo`:
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
| Request termination | `/status/{code}` | Custom error responses |
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
| Request size limiting | `/echo` | Body size in echo response |
| IP restriction | `/ip` | Client IP detection |

## gRPC

Rustybin serves a gRPC `EchoService` on a separate port (default `50051`,
`RUSTYBIN_GRPC_PORT`) for testing API gateway gRPC proxying (unary, streaming,
and web/HTTP transcoding). It implements all four call types — unary, server
streaming, client streaming, and bidirectional streaming — echoing the request
message along with the reflected request metadata and the handling instance ID.

The service definition lives in [`proto/echo.proto`](proto/echo.proto). Test it
with [`grpcurl`](https://github.com/fullstorydev/grpcurl):

```bash
# Unary
grpcurl -plaintext -d '{"message":"ping"}' \
  localhost:50051 rustybin.echo.v1.EchoService/Echo

# Server streaming — emit 5 responses
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
