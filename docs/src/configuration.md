# Configuration

Rustybin is configured only through environment variables. Invalid values never
stop the server: they are logged as warnings and the default is used. Values are
trimmed, and an empty value counts as unset (except for `RUSTYBIN_CORS_ORIGINS`, see
below). Booleans accept `1`, `true`, `yes`, `on` and `0`, `false`, `no`, `off`
(case-insensitive).

`GET /_rustybin/config` returns the effective configuration without secrets (the
admin token is only reported as `admin_token_configured`).

This list is checked against `src/config.rs` and every `std::env::var` call in
`src/` (the MCP, mock LLM and A2A modules read a few variables of their own).

## Listeners and process

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_HTTP_PORT` | `80` | HTTP port. Failing to bind it is fatal (exit code 1). `0` picks a free port. |
| `RUSTYBIN_HTTPS_PORT` | `443` | HTTPS port (same routes as HTTP). Optional: a bind or certificate failure only logs a warning. |
| `RUSTYBIN_GRPC_PORT` | `50051` | gRPC port (`EchoService`, health, reflection, gRPC-Web). Optional, like HTTPS. |
| `RUSTYBIN_HOST` | `0.0.0.0` | Bind address for all listeners: an IPv4 or IPv6 address (`::`, `[::]`) or `localhost` (127.0.0.1). |
| `RUSTYBIN_LOG_LEVEL` | `info` | Log filter in `tracing` `EnvFilter` syntax (`debug`, `warn`, `rustybin=debug,tower_http=info`, ...). Falls back to `RUST_LOG`; an unparsable filter means `info`. Logs go to stdout, coloured only when stdout is a terminal. |
| `RUSTYBIN_INSTANCE_ID` | random UUID | Instance name returned by `/identity`, `/health`, the mock LLM (`X-Rustybin-Instance`) and the gRPC `EchoService`. Set it per replica for load balancing demos. |
| `RUSTYBIN_REQUEST_TIMEOUT` | `120` | Seconds until the response headers must be ready, else `503`. Streaming bodies (SSE, WebSocket, slow drips) are not cut once headers are sent. `0` disables the timeout. |
| `RUSTYBIN_BODY_LIMIT` | `1048576` | Maximum request body in bytes; larger bodies get `413`. Also the size up to which `/echo` shows a body. |

The process stops gracefully on SIGTERM or SIGINT: listeners stop accepting and
in-flight connections (streams included) get 10 seconds to finish.

## Proxies and TLS

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_TRUST_FORWARD` | `false` | Trust proxy headers. Client IP: `Fly-Client-IP`, then RFC 7239 `Forwarded: for=`, `X-Forwarded-For` (the rightmost address that is not private or loopback, so clients cannot spoof it by prepending), then `X-Real-IP`. Scheme, host and port: `X-Forwarded-Proto`, `X-Forwarded-Host`, `X-Forwarded-Port` and `Forwarded: proto= host=`. The OIDC issuer also honours `X-Forwarded-Prefix`. Enable it only behind a proxy you control. |
| `RUSTYBIN_TLS_CERT` | `certs/server.crt` | Certificate of the HTTPS listener (relative to the working directory). |
| `RUSTYBIN_TLS_KEY` | `certs/server.key` | Private key of the HTTPS listener. |
| `RUSTYBIN_MTLS_IN_HEADER` | unset | Name of a request header in which a gateway that terminated mTLS forwards the client certificate (URL-encoded PEM, raw PEM or base64 DER). `/auth/mtls` then verifies it against the demo CA. See [Authentication](reference/auth.md#mtls). |

**Demo PKI.** At startup Rustybin loads a demo CA from `ca.crt` and `ca.key` in the
directory of `RUSTYBIN_TLS_CERT`, or generates one and writes it there (best effort),
so client certificates issued before a restart stay valid. Rustybin only reuses or
replaces files it created (a CA certificate with the subject `CN=Rustybin Demo CA`): if
`ca.crt` / `ca.key` hold another CA, such as your own, they are left untouched with a
warning and the demo CA lives in `rustybin-demo-ca.crt` / `rustybin-demo-ca.key` instead
(if those names are taken by something else too, it is kept in memory only). For the
HTTPS listener:

- if both `RUSTYBIN_TLS_CERT` and `RUSTYBIN_TLS_KEY` exist, they are used (a stale
  pair issued by an older demo CA is replaced; a key that does not match the
  certificate is an error and HTTPS does not start);
- if neither exists, a server certificate for `localhost`, `127.0.0.1`, `::1` and
  `rustybin` signed by the demo CA is generated and written there;
- if only one exists, nothing is written and the generated pair is used in memory.

Download the CA and a client certificate from `/auth/mtls/get-ca-cert` and
`/auth/mtls/get-client-cert`. The HTTPS listener asks for a client certificate but
does not require one; when one is presented it must chain to the demo CA.

## Shared instances

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_PUBLIC_MODE` | `false` | Hardening for shared, internet-facing instances: lower caps everywhere, the inspector only captures requests tagged with `X-Rustybin-Session`, and instance-global mutations are refused unless an admin token is configured. See [Sessions, public mode and the admin token](concepts/sessions.md). |
| `RUSTYBIN_ADMIN_TOKEN` | unset | When set, instance-global mutations (health toggles, `POST /flaky/reset?scope=all`, clearing every captured request) need `Authorization: Bearer <token>` or `X-Rustybin-Admin-Token: <token>`. Never returned by any endpoint. |
| `RUSTYBIN_CORS_ORIGINS` | `*` | Allowed CORS origins, comma separated. `off` or an empty value removes the CORS layer entirely, so a gateway's own CORS plugin can be demonstrated. |
| `RUSTYBIN_INSPECTOR_CAPACITY` | `500` | Requests kept by the [request inspector](concepts/inspector.md) ring buffer (1 to 10000). |

## MCP server

Read when the server starts.

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_MCP_API_KEY` | unset | The only `X-API-Key` value `/mcp/apikey` accepts. Unset: any non-empty key is accepted. |
| `RUSTYBIN_MCP_ALLOWED_ORIGINS` | `*` | Comma-separated `Origin` values accepted by every MCP endpoint (DNS rebinding protection). Requests with another `Origin` get `403`; requests without `Origin` are always accepted. |
| `RUSTYBIN_MCP_ACCEPTED_AUDIENCES` | `rustybin` | Token audiences `/mcp/protected` accepts besides its own resource URL. The default is the identity provider's default audience, so a plain `client_credentials` token works. `none` accepts only tokens issued for the resource (strict RFC 8707). |
| `RUSTYBIN_MCP_RESOURCE_URL` | derived | Override the resource identifier of `/mcp/protected` (by default `<scheme>://<host>/mcp/protected` from the request). Set it to the gateway's public URL when the gateway does not forward `Host`. |
| `RUSTYBIN_MCP_CLOCK_TICK_SECS` | `5` | Update interval of the subscribable `rustybin://clock` resource (1 to 3600). |

## Mock LLM

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_AI_REQUIRE_AUTH` | `false` | Require each provider's native credential on every `/ai/*` call (per request: `X-Rustybin-Require-Auth: true`). |
| `RUSTYBIN_AI_API_KEY` | unset | The only accepted key (implies `RUSTYBIN_AI_REQUIRE_AUTH`). For Bedrock SigV4 it is compared with the access key id. |

See [Faults, latency, credentials, inspection](ai/gateway-features.md#credential-checks).

## A2A agents

| Variable | Default | Description |
|---|---|---|
| `RUSTYBIN_A2A_PUSH_ALLOWLIST` | unset | Comma-separated `host` or `host:port` entries that push notification URLs may point to. The built-in sink `/a2a/webhook-sink/{id}` on this server is always allowed. |
| `RUSTYBIN_A2A_PUSH_ALLOW_ALL` | `false` | Allow push notifications to any http(s) URL, including loopback and private addresses. Ignored in public mode. |

## Development only

| Variable | Used by |
|---|---|
| `RUSTYBIN_UPDATE_README` | `RUSTYBIN_UPDATE_README=1 cargo test readme_endpoints` rewrites the endpoint tables in `README.md`. |
| `RUSTYBIN_BIN`, `RUSTYBIN_DOCS_PORT`, `REQUIRE_ALL_TOOLS` | `docs/examples/run.sh` (see [How the examples work](examples.md)). |
| `PYTHON`, `RUSTYBIN_CONFORMANCE_PORT`, `RUSTYBIN_URL`, `MCP_BASE` | The SDK conformance scripts under `conformance/`. |

## Limits that are not configurable

Request-reachable state is always bounded. The main caps (normal mode / public mode):

| What | Normal | Public |
|---|---|---|
| `X-Rustybin-Delay`, `/drip` duration and delay, JSON-RPC `sleep`, bin response delay | 30 s | 10 s |
| `/delay/{duration}` | 60 s | 10 s |
| `/bytes`, `/stream-bytes`, `/range` size | 100 KiB | 10 KiB |
| `/stream/{n}` lines | 100 | 20 |
| `/drip` bytes | 10240 | 1024 |
| `/sse` events / interval / stream lifetime | 1000 / 60 s / 10 min | 100 / 10 s / 2 min |
| `/sse/chat` tokens / delay per token | 500 / 2 s | 200 / 1 s |
| JSON-RPC batch size | 100 | 20 |
| WebSocket message size / idle timeout / lifetime | 1 MiB / 5 min / 1 h | 256 KiB / 1 min / 10 min |
| Request bins (total / per session / requests per bin / bytes per bin / TTL) | 200 / 200 / 100 / 1 MiB / 24 h | 100 / 10 / 50 / 256 KiB / 1 h |
| Flaky counters (idle TTL 1 h) | 10000 | 2000 |
| OAuth codes, refresh tokens, revocations / registered clients | 10000 / 1000 | 2000 / 200 |
| Mock LLM request records (1 h TTL) / choices `n` / embedding dimensions | 1000 / 8 / 4096 | 200 / 2 / 1536 |
| MCP sessions (idle TTL) / long-lived streams | 1000 (30 min) / 1 h | 200 (10 min) / 5 min |
| A2A tasks (total / per session / idle TTL) | 5000 / 1000 / 1 h | 2000 / 50 / 15 min |
| Inspector entries | `RUSTYBIN_INSPECTOR_CAPACITY` (max 10000) | same |
