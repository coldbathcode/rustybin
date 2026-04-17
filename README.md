# Rustybin

A high-performance, all-in-one HTTP stub/echo service written in Rust using [axum](https://github.com/tokio-rs/axum). Rustybin is designed to exercise every category of API gateway plugin — specifically [API gateway](https://example.com/) — from a single binary. Think httpbin, but faster, broader, and purpose-built for gateway demos and testing.

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
| **Health & Identity** | `/health`, `/identity` | Service health check and instance identity (hostname, uptime, request count) |
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
| **OIDC Provider** | `/.well-known/openid-configuration`, `/oauth/token`, `/oauth/jwks`, `/oauth/authorize`, `/oauth/userinfo`, `/oauth/introspect` | Full OpenID Connect Identity Provider |
| **mTLS** | `/auth/mtls`, `/auth/mtls/get-client-cert`, `/auth/mtls/get-ca-cert` | Mutual TLS with demo PKI |
| **AI Gateway** | `/ai/v1/chat/completions`, `/ai/v1/completions`, `/ai/v1/embeddings`, `/ai/v1/models` | OpenAI-compatible endpoints for AI gateway testing |
| **GraphQL** | `/graphql`, `/graphql/schema` | GraphQL API with playground (users, products, orders) |
| **Orchestration** | `/orchestration/step/1-4`, `/orchestration/status` | Multi-step payment processing pipeline for orchestration |
| **SOAP** | `/soap`, `/soap/wsdl` | SOAP/XML web service with WSDL |
| **Flaky** | `/flaky/{rate}`, `/flaky/pattern/{p}`, `/flaky/after/{n}`, `/flaky/recover/{n}` | Configurable failure simulation for circuit breaker testing |
| **Docs** | `/openapi.json`, `/openapi.yaml`, `/docs` | OpenAPI 3.0.3 spec and interactive Scalar UI |

All JSON endpoints support content negotiation — send `Accept: application/xml` for XML responses.

## Configuration

| Environment Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_HTTP_PORT` | `80` | HTTP listen port |
| `RUSTYBIN_HTTPS_PORT` | `443` | HTTPS listen port |
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

Each instance returns its ID via `/identity`, making it easy to see which upstream Gateway selected.

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

## API gateway Integration

### Upstream Configuration

```yaml
# gateway.yml (declarative config)
services:
  - name: rustybin
    url: http://rustybin:80
    routes:
      - name: rustybin-route
        paths:
          - /
```

### Plugin Testing Examples

**Rate Limiting** — hit `/echo` to verify rate limit headers:
```bash
curl -i http://gateway:8000/echo
# Look for X-RateLimit-Remaining headers
```

**Basic Auth** — configure the gateway's basic-auth plugin, upstream validates at `/auth/basic-auth`:
```bash
curl -u alice:secret http://gateway:8000/auth/basic-auth/alice/secret
```

**JWT** — get a token from the OIDC provider, validate through Gateway:
```bash
TOKEN=$(curl -s -X POST http://rustybin/oauth/token \
  -d 'grant_type=client_credentials&client_id=rustybin&client_secret=secret' \
  | jq -r .access_token)
curl -H "Authorization: Bearer $TOKEN" http://gateway:8000/auth/jwt
```

**Circuit Breaker** — use `/flaky/50` to trigger the gateway's circuit breaker:
```bash
for i in $(seq 1 20); do curl -s -o /dev/null -w "%{http_code}\n" http://gateway:8000/flaky/50; done
```

**Response Transformation** — inspect headers added by Gateway via `/echo`:
```bash
curl -s http://gateway:8000/echo | jq .headers
```

## Plugin Testing Matrix

| Gateway Plugin | Rustybin Endpoint | What to Test |
|---|---|---|
| rate-limiting | `/echo`, `/anything` | Rate limit headers in echo response |
| basic-auth | `/auth/basic-auth/{u}/{p}` | Credential forwarding |
| key-auth | `/auth/api-key/{header}/{key}` | API key forwarding |
| jwt | `/auth/jwt`, `/oauth/jwks` | JWT validation, JWKS endpoint |
| oauth2 / openid-connect | `/.well-known/openid-configuration` | Full OIDC flow |
| mtls-auth | `/auth/mtls` | Client certificate validation |
| request-termination | `/status/{code}` | Custom error responses |
| proxy-cache | `/cache/{ttl}` | Cache headers, conditional requests |
| response-transformer | `/echo` | Header/body inspection |
| retry | `/flaky/{rate}`, `/flaky/pattern/{p}` | Retry on 503 |
| circuit-breaker | `/flaky/after/{n}`, `/flaky/recover/{n}` | Health state transitions |
| ai-proxy | `/ai/v1/chat/completions` | OpenAI-compatible upstream |
| graphql-proxy | `/graphql` | GraphQL query proxying |
| soap-xml | `/soap`, `/soap/wsdl` | SOAP envelope routing |
| orchestration | `/orchestration/step/1-4` | Multi-step orchestration |
| redirect | `/redirect/{n}` | Redirect following |
| cors | `/echo` | CORS header inspection |
| request-size-limiting | `/echo` | Body size in echo response |
| ip-restriction | `/ip` | Client IP detection |

## API Documentation

Visit `/docs` for interactive API documentation powered by [Scalar](https://github.com/scalar/scalar), or fetch the raw spec:

```bash
curl http://localhost/openapi.json  # OpenAPI 3.0.3 JSON
curl http://localhost/openapi.yaml  # YAML format
```
