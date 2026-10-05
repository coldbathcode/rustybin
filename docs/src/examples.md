# How the examples work

Every request shown in this documentation is a tested example. The examples live
as [Hurl](https://hurl.dev) files under `docs/examples/` (plus a few shell scripts
for gRPC, WebSocket and TLS client certificates, which Hurl cannot drive), and the
pages include them directly, so what you read is what CI runs.

A Hurl entry is a plain HTTP request followed by the expected response:

```hurl
{{#include ../examples/http/echo.hurl:echo_post}}
```

- `{{base_url}}` is the Rustybin base URL (`http://localhost:8080` in your setup).
- `HTTP 200` asserts the status; the `[Asserts]` section checks headers and the body
  (`jsonpath`, `xpath`, `body contains`, `duration`, ...).
- `[Captures]` stores a value (a token, an id) for the next requests, which is how
  the OAuth, MCP session and A2A examples chain calls.
- `[Form]`, `[BasicAuth]`, `[Query]` and `[Multipart]` are shorthands for form
  bodies, `Authorization: Basic`, query parameters and multipart uploads.

The equivalent `curl` for the entry above is:

```bash
curl -s http://localhost:8080/anything -H 'Content-Type: application/json' \
  -d '{"message": "hello", "number": 42}'
```

Shell examples (gRPC, WebSocket, TLS client certificates, never-ending feeds) use
variables for the target: `$BASE` (for example `http://localhost:8080`), `$HTTP` and
`$HTTPS` (`https://localhost:8443`), `$WS` (`ws://localhost:8080`) and `$GRPC`
(`localhost:50051`).

## Running them

```bash
docs/examples/run.sh                       # every example
docs/examples/run.sh ai/openai.hurl        # one file (paths relative to docs/examples)
```

`run.sh` builds Rustybin (or uses `RUSTYBIN_BIN`), starts a fresh instance on ports
18800 (HTTP), 18801 (HTTPS) and 18802 (gRPC), runs the files and stops the server.
A second instance on port 18803 (Hurl variable `plan_url`) runs the free plan with
tiny overrides for the [plan limit](concepts/plans-and-limits.md) examples. A
third instance on ports 18804 (HTTP, Hurl variable `secured_url`) and 18805 (HTTPS)
runs as a hosted platform would: `RUSTYBIN_HOSTED_MODE=true` with a signing key
generated for the run, `RUSTYBIN_GRPC_ON_HTTP=true`, a console title and back link
(the [control-plane security](concepts/control-plane-security.md) and
[gRPC on HTTP](reference/grpc.md#on-the-http-and-https-ports) examples). Its tokens
are minted with `concepts/_mint-control-jwt.sh` and passed as the Hurl variables
`console_token`, `inspector_token`, `expired_token` and `other_audience_token`. The
main instance uses these settings, which some examples rely on:

| Setting | Value | Why |
|---|---|---|
| `RUSTYBIN_ADMIN_TOKEN` | `docs-admin-token` (Hurl variable `admin_token`) | to show admin-guarded mutations |
| `RUSTYBIN_MTLS_IN_HEADER` | `X-Client-Cert` | to show mTLS header mode |
| `RUSTYBIN_INSTANCE_ID` | `docs-examples` | stable identity in assertions |
| `RUSTYBIN_TLS_CERT` / `RUSTYBIN_TLS_KEY` | a temporary directory | the demo PKI is never written into the repository |

Requirements: `hurl` and `curl`; `grpcurl`, `websocat` and `jq` for the shell
examples (skipped with a notice when missing, or a failure with
`REQUIRE_ALL_TOOLS=1`, which CI sets), and `openssl` 3 for the signing key. Set
`RUSTYBIN_DOCS_PORT` to move the six ports.

To run a single Hurl file against your own instance:

```bash
hurl --test --variable base_url=http://localhost:8080 \
  --variable admin_token=change-me docs/examples/http/echo.hurl
```
