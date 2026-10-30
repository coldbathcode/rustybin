# Quick start

All examples on this page assume Rustybin answers on `http://localhost:8080`.

## Docker

There is no published image: build it from the repository (the build compiles a
release binary, which takes a few minutes the first time).

The published image needs no build:

```bash
docker run -d --name rustybin -p 8080:80 -p 8443:443 -p 50051:50051 ghcr.io/coldbathcode/rustybin:latest
```

`latest` and `edge` follow the main branch, a release `vX.Y.Z` also publishes `X.Y.Z`, `X.Y`
and `X`, and every build publishes `sha-<commit>`; the image is multi-arch (amd64 and
arm64) and every push is signed with cosign, keyless, by the publishing workflow's GitHub
identity, so you can check what you pull:

```bash
cosign verify ghcr.io/coldbathcode/rustybin:latest \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-identity-regexp '^https://github.com/coldbathcode/rustybin/'
```

Or build it yourself:

```bash
git clone https://github.com/coldbathcode/rustybin && cd rustybin
docker build -t rustybin .
docker run -d --name rustybin -p 8080:80 -p 8443:443 -p 50051:50051 rustybin
```

The container listens on 80 (HTTP), 443 (HTTPS) and 50051 (gRPC) and runs as an
unprivileged user (uid 10001). On first start it generates a demo PKI in `/app/certs`
(a CA, the HTTPS server certificate and a client certificate for mTLS demos); mount
a volume on `/app/certs` to keep it across restarts (a bind-mounted host directory
must be writable by uid 10001). The image also runs with `--read-only` when
`/app/certs` is a volume, see [Containers](concepts/control-plane-security.md#containers).
Settings are environment variables, see [Configuration](configuration.md):

```bash
docker run -d -p 8080:80 -e RUSTYBIN_INSTANCE_ID=demo-1 -e RUSTYBIN_ADMIN_TOKEN=change-me rustybin
```

## First sign-in

The [web console](console.md) at `http://localhost:8080/ui` and the control plane
(`/_rustybin/*`) ask for a user name and password: **`rustybin` / `rustybin`**. The
instance warns in its log and the console shows a banner until you change it under
**Management, Sign-in** (the current password is required; the new one is stored as
a hash, and every other session ends). Or set it before the first start:

```bash
docker run -d -p 8080:80 -e RUSTYBIN_CONSOLE_PASSWORD=change-me rustybin
```

Scripts sign in the same way and get a token that opens everything, the
[management MCP server](agent-mcp.md) included:

```bash
TOKEN=$(curl -s http://localhost:8080/_rustybin/login -H 'Content-Type: application/json' \
  -d '{"username":"rustybin","password":"rustybin"}' | jq -r .token)
curl -s http://localhost:8080/_rustybin/status -H "Authorization: Bearer $TOKEN"
```

`RUSTYBIN_CONTROL_AUTH=open` turns the sign-in off for a laptop or a lab; the data
plane (everything your gateway calls) never needs credentials. Details:
[Control-plane security](concepts/control-plane-security.md).

## Five commands

The same binary is the [command line](cli-guide.md). From nothing to a mock written
by your AI, published for testers and exported as code:

```bash
# 1. Sign in (rustybin / rustybin); the token lands in a profile.
rustybin login --url http://localhost:8080

# 2. Your own AI key (Anthropic here; openai, gemini or compatible work the same).
rustybin settings set assist.provider anthropic
rustybin settings set-secret assist.anthropic_key --stdin < key.txt

# 3. Describe the API; the spec is checked and deployed as the mock "orders".
rustybin mocks describe "an orders API with customers, orders and refunds" > orders.json
rustybin mocks create orders --spec orders.json

# 4. Publish it: testers open http://localhost:8080/portal, no password needed.
rustybin portal publish mock:orders --title "Orders"

# 5. The whole instance as code (secrets as references), ready for git.
rustybin config export > rustybin.yaml
```

The console does the same: **Management, AI** for the key, the mock wizard's
**Describe it** tab, **Publish to portal** on the mock, **Management, Export**; and
an agent does it through the [management MCP server](agent-mcp.md) with the
`build_instance` prompt. Without a key, step 3 is `rustybin mocks create orders
--spec your-spec.yaml` (or the built-in `petstore`), and everything else is the same.

## Docker Compose

`docker-compose.yml` builds the image, publishes ports 80, 443 and 50051 and keeps
the demo PKI in the named volume `rustybin-certs` so it survives restarts (download
the CA with `curl -o ca.crt http://localhost/auth/mtls/get-ca-cert`):

```bash
docker compose up -d
curl http://localhost/health
```

The file also contains a commented-out block for two more instances
(`rustybin-02`, `rustybin-03`) with their own `RUSTYBIN_INSTANCE_ID`, for load
balancing demos (see [Identity and load balancing](reference/identity.md)).

## Cargo

Rust 1.88 or newer. The default ports (80 and 443) need root, so pick high ports:

```bash
RUSTYBIN_HTTP_PORT=8080 RUSTYBIN_HTTPS_PORT=8443 cargo run --release
```

`cargo run` writes the demo PKI to `certs/` in the working directory (the default
`RUSTYBIN_TLS_CERT` is `certs/server.crt`); the files are git-ignored. Other flags:

```bash
cargo run -- --version                    # print the version
cargo run -- --print-endpoints-markdown   # print the endpoint tables (from the route catalogue)
cargo run -- --config rustybin.yaml       # run with OpenAPI mocks declared in a file
cargo run -- mock --help                  # the OpenAPI mock CLI
```

See [OpenAPI mocks](mocks/index.md) for `rustybin.yaml`, mounting specs into the
container and the `rustybin mock` commands.

## Fly.io

`fly.toml` deploys the Dockerfile with the web surface behind Fly's edge TLS and the
gRPC port as raw TCP passthrough:

```bash
fly launch --copy-config --no-deploy   # once: pick an app name and region
fly deploy
```

Things to know about the provided configuration:

- `RUSTYBIN_TRUST_FORWARD=true`: Fly's proxy sets `Fly-Client-IP` and
  `X-Forwarded-*`, so `/ip`, `/echo` and the OIDC issuer report the real client and
  `https` scheme.
- The HTTP health check targets `GET /_rustybin/ready` (always 200 while serving),
  not `/health`, which is a demo toggle that can return 503.
- The image runs as an unprivileged user, so the configuration moves the listeners
  to ports 8080 (HTTP, Fly's `internal_port`) and 8443 (HTTPS).
- To serve gRPC through Fly's TLS edge on 443 instead of a raw port, set
  `RUSTYBIN_GRPC_ON_HTTP=true` and give the 443 service an HTTP/2 backend (see
  [gRPC](reference/grpc.md#on-the-http-and-https-ports)).
- Because Fly terminates TLS for the web surface, Rustybin's own HTTPS listener and
  mTLS on it are not reachable; use the `RUSTYBIN_MTLS_IN_HEADER` mode for mTLS demos
  behind a gateway instead (see [Authentication](reference/auth.md#mtls)).
- gRPC on port 50051 is plaintext h2c: `grpcurl -plaintext <app>.fly.dev:50051 list`.
- For a shared, internet-facing instance set `RUSTYBIN_PUBLIC_MODE=true` (see
  [Sessions, public mode and the admin token](concepts/sessions.md)).

## First requests

```bash
# Echo: see exactly what arrived (method, path, query, headers, body)
curl -s 'http://localhost:8080/anything/hello?demo=1' -H 'X-Demo: rustybin'

# Mock LLM, OpenAI wire format
curl -s http://localhost:8080/ai/openai/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"hello"}]}'

# An OpenAPI mock: the built-in Petstore sample (upload your own spec in /ui/#/mocks)
curl -s http://localhost:8080/mock/petstore/v1/pets/42

# MCP: start a session (2025-06-18 handshake)
curl -si http://localhost:8080/mcp -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"curl","version":"1"}}}'

# A2A: the default agent card
curl -s http://localhost:8080/.well-known/agent-card.json

# Chaos on any route
curl -si http://localhost:8080/echo -H 'X-Rustybin-Fail: 503'

# An access token from the built-in identity provider
curl -s http://localhost:8080/oauth/token \
  -d grant_type=client_credentials -d client_id=rustybin -d client_secret=secret
```

<details>
<summary>The same requests as a tested Hurl file</summary>

```hurl
{{#include ../examples/quickstart/quickstart.hurl:echo}}

{{#include ../examples/quickstart/quickstart.hurl:chat}}

{{#include ../examples/quickstart/quickstart.hurl:mock}}

{{#include ../examples/quickstart/quickstart.hurl:mcp}}

{{#include ../examples/quickstart/quickstart.hurl:a2a}}

{{#include ../examples/quickstart/quickstart.hurl:fault}}

{{#include ../examples/quickstart/quickstart.hurl:token}}
```

</details>

Then open `http://localhost:8080/` for the endpoint list with runnable examples,
`/docs` for the interactive API reference and `/ui` for the
[web console](console.md).
