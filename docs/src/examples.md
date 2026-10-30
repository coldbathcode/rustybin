# How the examples work

The reference examples are executable tests. The examples live
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
(`localhost:50051`). The [mock CLI](mocks/cli.md) examples call the `rustybin`
binary (`$RUSTYBIN_BIN` when run by `run.sh`).

## Run one example against your instance

From a repository checkout, install Hurl 8+ and save a block as a `.hurl` file,
or use the checked-in example directly:

```bash
hurl --test --variable base_url=http://localhost:8080 \
  docs/examples/http/echo.hurl
```

This echo example works on a default instance. Control-plane examples require
sign-in unless the instance uses `RUSTYBIN_CONTROL_AUTH=open`; the full runner
below supplies its own setup and credentials. Follow a file's earlier requests
when later entries use captured variables.

## Running them

Run the suite from a repository checkout with **Bash 4+**, Hurl 8+, curl and
OpenSSL 3. On macOS the system Bash is too old; use a current Bash (for example,
`brew install bash hurl openssl`) and invoke `/opt/homebrew/bin/bash docs/examples/run.sh`
on Apple Silicon, or `/usr/local/bin/bash docs/examples/run.sh` on Intel.

```bash
docs/examples/run.sh                       # every example
docs/examples/run.sh ai/openai.hurl        # one file (paths relative to docs/examples)
```

`run.sh` builds Rustybin (or uses `RUSTYBIN_BIN`), starts a fresh instance on ports
18800 (HTTP), 18801 (HTTPS) and 18802 (gRPC), runs the files and stops the servers.
A second instance on port 18803 (Hurl variable `plan_url`) runs the free plan with
tiny overrides for the [plan limit](concepts/plans-and-limits.md) examples. A
third instance on ports 18804 (HTTP, Hurl variable `secured_url`) and 18805 (HTTPS)
runs as a hosted platform would: `RUSTYBIN_HOSTED_MODE=true` with a signing key
generated for the run, `RUSTYBIN_GRPC_ON_HTTP=true`, a console title and back link
(the [control-plane security](concepts/control-plane-security.md) and
[gRPC on HTTP](reference/grpc.md#on-the-http-and-https-ports) examples). Its tokens
are minted with `concepts/_mint-control-jwt.sh` and passed as the Hurl variables
`console_token`, `inspector_token`, `expired_token` and `other_audience_token`. Further
instances serve the [shared mode](concepts/shared-mode.md), `RUSTYBIN_REQUIRE_KEY` and
[self-service](mocks/self-service.md) examples (`shared_url`, `keyed_url`, `ss_url`), and
one on port 18809 runs in [harness mode](harness.md) (`harness_url`: roles `both`
and `coordinator`, loop-through, 127.0.0.1 allowed, one engine thread), so the load test
examples send their requests to that instance itself, and a last one on port 18810
(`password_url`) starts with nothing set, so it asks for `rustybin` / `rustybin`
(the [sign-in](concepts/control-plane-security.md#user-name-and-password) examples).
Two shell examples start instances of their own: `config/source-control.sh` runs three
against a bare git repository in a temporary directory (one that commits and pushes,
one that starts from the repository, one connected through the settings), and
`cli/cli.sh` signs in to an instance of its own with the default password and drives the
command line against it.
The main instance uses these settings, which some examples rely on:

| Setting | Value | Why |
|---|---|---|
| `RUSTYBIN_CONTROL_AUTH` | `open` | the examples read the control plane without a token (the default, the password sign-in, is the last instance's) |
| `RUSTYBIN_ADMIN_TOKEN` | `docs-admin-token` (Hurl variable `admin_token`) | to show admin-guarded mutations |
| `RUSTYBIN_ASSIST_PROVIDER` | `mock` | the [AI assist](ai-assist.md#the-mock-provider) examples run against the built-in provider: deterministic answers, no key, no network |
| `RUSTYBIN_MTLS_IN_HEADER` | `X-Client-Cert` | to show mTLS header mode |
| `RUSTYBIN_INSTANCE_ID` | `docs-examples` | stable identity in assertions |
| `RUSTYBIN_CONFIG_FILE` / `RUSTYBIN_MOCKS_DIR` | `mocks/rustybin.yaml` / `mocks/specs-dir` (with `INVENTORY_API_KEY=docs-inventory-key`) | the [OpenAPI mocks loaded from files](mocks/config.md) |
| `RUSTYBIN_TLS_CERT` / `RUSTYBIN_TLS_KEY` | a temporary directory | the demo PKI is never written into the repository |

Requirements: `hurl` and `curl`; `grpcurl`, `websocat` and `jq` for the shell
examples (skipped with a notice when missing, or a failure with
`REQUIRE_ALL_TOOLS=1`, which CI sets), and `openssl` 3 for the signing key. Set
`RUSTYBIN_DOCS_PORT` to move the ports (18800 to 18810).
