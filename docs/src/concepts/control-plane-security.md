# Control-plane security, metrics and operations

The control plane (`/_rustybin/*`) and the [web console](../console.md) are locked by
default: a fresh instance asks for a user name and password, `rustybin` / `rustybin`
out of the box, and warns in its log and in the console until the password is
changed. The data plane (everything outside `/_rustybin/`) is never affected: it is
the product your gateway calls.

```bash
rustybin                                         # sign in with rustybin / rustybin, then change it
RUSTYBIN_CONSOLE_PASSWORD=change-me rustybin     # or set it before the first start
RUSTYBIN_CONTROL_AUTH=open rustybin              # a laptop or a lab: no sign-in, as before 0.2
```

## Modes

| `RUSTYBIN_CONTROL_AUTH` | Control plane (`/_rustybin/*`) accepts |
|---|---|
| `password` (default) | A token from the [sign-in](#user-name-and-password) with the console's user name and password, plus everything `jwt` mode accepts (the admin token, control JWTs when a key is configured). `jwt` is the default in [hosted mode](#hosted-mode), `open` in [shared mode](shared-mode.md), where API keys authenticate the control plane. |
| `open` | Anyone. Single mutations still follow the [admin token rules](sessions.md). The default before 0.2; [self-service](../mocks/self-service.md) instances choose it on purpose. |
| `token` | Only `RUSTYBIN_ADMIN_TOKEN`, as `Authorization: Bearer <token>` or `X-Rustybin-Admin-Token: <token>`. |
| `jwt` | The admin token, or an Ed25519 (EdDSA) JWT signed by the key in `RUSTYBIN_CONTROL_JWT_PUBLIC_KEY`. |

Always reachable without credentials, in every mode:

- `GET /_rustybin/ready`, the readiness probe (see below), and
  `POST /_rustybin/login`, the sign-in itself;
- the data plane, including `GET /` (liveness) and `/health` (the demo toggle);
- the console's static files under `/ui/`. They are the same open-source files for
  every instance and carry no instance data: a browser must be able to load them to
  show the sign-in screen or pick up a token from the URL fragment, which never
  reaches the server. Everything the console shows comes from `/_rustybin/*` with the
  token.

Without a token the control plane answers `401` with a `WWW-Authenticate: Bearer`
challenge and a JSON body naming the reason (`missing_token`, `expired`,
`invalid_audience`, `invalid_signature`, `invalid_algorithm`, `not_yet_valid`,
`missing_claim`, `malformed`, `revoked`, `invalid_token`):

```hurl
{{#include ../../examples/concepts/login.hurl:locked}}
```

Unauthenticated control-plane requests are rejected before [plan limits](plans-and-limits.md)
count them; authenticated ones are counted but never rejected by the limiter.
`token` mode without `RUSTYBIN_ADMIN_TOKEN` (or `jwt` mode with neither a key nor an
admin token) refuses every control-plane request and logs a warning at startup. The
same lock in `jwt` mode, as a hosted platform runs it:

```hurl
{{#include ../../examples/concepts/control_auth.hurl:no_token}}
```

```hurl
{{#include ../../examples/concepts/control_auth.hurl:open_routes}}
```

## User name and password

In `password` mode the console's sign-in screen, the [`rustybin` CLI](../cli-guide.md)
and any script sign in with `POST /_rustybin/login` and get a **control token**: an
HS256 JWT signed with the instance key (the one the [OAuth sign-in](#agents-sign-in-with-oauth)
uses), scope `admin`, valid for 12 hours, carrying a session id and the token
generation. It is a control JWT like any other, so the whole control plane, the
[management MCP server](../agent-mcp.md) and the data plane's admin guard accept it
as `Authorization: Bearer`, and captures redact it.

| Setting | Default | Where |
|---|---|---|
| `RUSTYBIN_CONSOLE_USER` | `rustybin` | The setting `console.user`; a value saved in the console wins over the variable |
| `RUSTYBIN_CONSOLE_PASSWORD` | `rustybin` | The password in the clear, for a first deployment |
| `RUSTYBIN_CONSOLE_PASSWORD_HASH` | unset | An argon2id PHC string (`$argon2id$v=19$...`), preferred for deployments; wins over the plain password. Seeds the secret `console.password_hash` |

A password changed in the console (**Management, Sign-in**, the current password
required) or through `PUT /_rustybin/password` is stored as an argon2id hash in the
[settings store](#settings-and-secrets-at-rest) (the secret `console.password_hash`)
and wins over the environment, unless `RUSTYBIN_SETTINGS_LOCK` makes the
environment authoritative. The change ends every other session; the caller gets a
fresh token. Forgot it? `DELETE /_rustybin/settings/console.password_hash` with the
admin token goes back to the environment's password or the default.

```hurl
{{#include ../../examples/concepts/login.hurl:wrong_password}}
```

```hurl
{{#include ../../examples/concepts/login.hurl:login}}
```

```hurl
{{#include ../../examples/concepts/login.hurl:use_token}}
```

```hurl
{{#include ../../examples/concepts/login.hurl:mcp}}
```

```hurl
{{#include ../../examples/concepts/login.hurl:change_password}}
```

```hurl
{{#include ../../examples/concepts/login.hurl:hash_in_settings}}
```

**Sessions.** `POST /_rustybin/logout` revokes the presented token for the rest of
its life (a bounded denylist of session ids, at most 4096, expiring with the
tokens). `GET /_rustybin/sessions` (admin) lists the sign-in tokens issued since the
last restart (at most 256: user, address, issue, last use and expiry times),
`DELETE /_rustybin/sessions/{id}` ends one, and `DELETE /_rustybin/sessions` signs out
everywhere by rotating the token generation every token carries, the caller's
included. `GET /_rustybin/session` describes the credential a request presents
(`obtained`: `password`, `admin_token`, `control_jwt` or `api_key`), which is how
the console shows who is signed in.

```hurl
{{#include ../../examples/concepts/login.hurl:sessions}}
```

```hurl
{{#include ../../examples/concepts/login.hurl:end_session}}
```

```hurl
{{#include ../../examples/concepts/login.hurl:logout}}
```

```hurl
{{#include ../../examples/concepts/login.hurl:sign_out_everywhere}}
```

**Brute force.** Five failed attempts from one client address within a minute, and
the address waits: `429` with `Retry-After`, a minute the first time, doubling up to
ten minutes. Every attempt is logged on the `rustybin::auth` target with the user
name, the address and the outcome, never the password. Passwords are compared in
constant time; hashes are verified off the request threads.

**The default password.** With it the instance logs a warning at startup,
`GET /_rustybin/status` and the sign-in answer carry `default_password: true`, and
the console shows a banner until it is changed. A public instance
(`RUSTYBIN_PUBLIC_MODE=true`) refuses to start with the default: set a password, or
choose `open` on purpose.

**Tokens across restarts.** Sign-in tokens are signed with the same key as the
OAuth sign-in: `RUSTYBIN_CONTROL_OAUTH_KEY`, else derived from `RUSTYBIN_ADMIN_TOKEN`,
else random per process. With a random key (and the default audience, a random
instance id) a restart signs everyone out; set one of them, and
`RUSTYBIN_INSTANCE_ID`, so sessions survive restarts and replicas.

**Migration from 0.1.** Instances that relied on an open control plane now ask for
`rustybin` / `rustybin`. Keep the old behaviour with `RUSTYBIN_CONTROL_AUTH=open`,
or set `RUSTYBIN_CONSOLE_PASSWORD`. `token` and `jwt` mode are unchanged. Platforms
in hosted mode keep `jwt`, shared instances keep `open` with API keys.

## Signed access tokens (`jwt` mode)

A portal, a CI job or a script that holds the private key issues short-lived tokens;
the instance only needs the public key. A token must have:

| Claim | Rule |
|---|---|
| header `alg` | `EdDSA` (Ed25519); any other algorithm is refused |
| `aud` | equal to `RUSTYBIN_CONTROL_JWT_AUDIENCE` (default: `RUSTYBIN_INSTANCE_ID`), so a token for one instance does not open another. With neither set the instance id is a fresh UUID on every start, so tokens stop working after a restart: the server warns at startup |
| `exp` | required; 30 seconds of clock skew are tolerated (also for `nbf` when present) |
| `scope` | optional: a space separated string or an array; absent means `console` |

| Scope | Grants |
|---|---|
| `inspector` | read-only `/_rustybin/requests*` (the [request inspector](inspector.md) and its live feed) |
| `console` | read-only `/_rustybin/*` (everything the console reads, metrics included) |
| `admin` | everything, including mutations; also accepted wherever the [admin token](sessions.md) is (health toggles, global resets) |

A mutation (any method other than GET, HEAD or OPTIONS) needs the admin token or the
`admin` scope; other scopes get `403` with `reason: insufficient_scope`. Two `POST`
routes change nothing and are open to the `console` scope: previewing a mock spec
(`/_rustybin/mocks/import`) and validating a body against a mock operation
(`/_rustybin/mocks/{name}/operations/{op}/validate`).

```hurl
{{#include ../../examples/concepts/control_auth.hurl:console_token}}
```

```hurl
{{#include ../../examples/concepts/control_auth.hurl:console_cannot_mutate}}
```

```hurl
{{#include ../../examples/concepts/control_auth.hurl:inspector_scope}}
```

```hurl
{{#include ../../examples/concepts/control_auth.hurl:refused_tokens}}
```

The admin token keeps working in `jwt` mode (for automation such as a usage
collector):

```hurl
{{#include ../../examples/concepts/control_auth.hurl:admin_token}}
```

### Keys and tokens with openssl

`RUSTYBIN_CONTROL_JWT_PUBLIC_KEY` takes a PEM public key (newlines may be written as
`\n`), the base64 of its DER, or the base64 / base64url of the raw 32 bytes.

```bash
openssl genpkey -algorithm ed25519 -out control.key
openssl pkey -in control.key -pubout -out control.pub
RUSTYBIN_CONTROL_AUTH=jwt RUSTYBIN_CONTROL_JWT_PUBLIC_KEY="$(cat control.pub)" \
  RUSTYBIN_CONTROL_JWT_AUDIENCE=team-demo rustybin
```

Minting a token needs nothing but openssl 3
([`_mint-control-jwt.sh`](https://github.com/coldbathcode/rustybin/blob/main/docs/examples/concepts/_mint-control-jwt.sh)
`<key> <audience> [scope] [ttl]`, used by the documentation runner):

```bash
{{#include ../../examples/concepts/_mint-control-jwt.sh:mint}}
```

Any JWT library with EdDSA support works too (PyJWT with `cryptography`,
`jsonwebtoken` in Rust, `jose` in JavaScript).

### The console with a token

In `password` mode the console's sign-in screen asks for the user name and
password (a pasted token is the alternative, folded under "Use a token instead");
in `token` and `jwt` mode it asks for a token. A token can also come in the URL
fragment: `https://rustybin.example.com/ui/#token=<jwt>` (or `#/traffic&token=<jwt>`
to land on a view). The console keeps the token in memory and in `sessionStorage`
(this browser tab only; **Remember me** keeps a sign-in token in `localStorage`
until it expires), removes it from the address bar at once, and sends it as
`Authorization: Bearer` on every control-plane call, including the live traffic
feed, which it reads with `fetch` rather than `EventSource` so the header can be
sent (no token in a query string). When the token is missing, refused, revoked or
expired (the console also watches the `exp` claim), it shows the sign-in screen
again, with a link back to `RUSTYBIN_CONSOLE_BACKLINK` when one is set. The top bar
shows who is signed in and when the session expires; **Management, General, Sign
out** revokes a sign-in token and forgets it.

### Agents sign in with OAuth

In `password`, `token` and `jwt` mode the management MCP server (`/_rustybin/mcp`) also lets an
AI agent sign in through the browser: its `401` points at RFC 9728 metadata, the
agent registers with Rustybin's built-in authorization server (`/_rustybin/oauth`),
and you approve it on the console's consent page with the credential the console
holds. The agent's token is capped by that credential's scopes and opens
`/_rustybin/mcp` only, never the rest of the control plane. `RUSTYBIN_CONTROL_OAUTH`
turns it off, `RUSTYBIN_CONTROL_OAUTH_KEY` keeps sign-ins across restarts and
replicas, and `RUSTYBIN_CONTROL_OAUTH_ISSUER` hands sign-in to an external
authorization server that issues control JWTs. See [Sign in with
OAuth](../agent-mcp.md#sign-in-with-oauth).

## Hosted mode

`RUSTYBIN_HOSTED_MODE=true` is a preset for platforms that run one instance per
customer: control auth `jwt` and JSON logs. Explicit `RUSTYBIN_CONTROL_AUTH` and
`RUSTYBIN_LOG_FORMAT` values win over the preset. A typical setup:

```bash
RUSTYBIN_HOSTED_MODE=true
RUSTYBIN_INSTANCE_ID=acme                       # also the default JWT audience
RUSTYBIN_CONTROL_JWT_PUBLIC_KEY="<platform public key>"
RUSTYBIN_ADMIN_TOKEN="<random, for the platform's own collector>"
RUSTYBIN_GRPC_ON_HTTP=true                      # gRPC through the HTTPS edge
RUSTYBIN_TRUST_FORWARD=true
RUSTYBIN_CONSOLE_TITLE="Acme demo"
RUSTYBIN_CONSOLE_BACKLINK=https://portal.example.com/instances/acme
RUSTYBIN_USAGE_FILE=/app/certs/usage.json       # on the writable volume
```

## Settings and secrets at rest

The instance settings (`/_rustybin/settings`, the console's Management view) hold
values that override environment variables and, for a few keys, secrets. With
`RUSTYBIN_STATE_DIR` they persist in `<dir>/settings.json` (mode `0600`, written
atomically); secrets are sealed with ChaCha20-Poly1305 under the key in
`RUSTYBIN_SECRETS_KEY` or `<dir>/secrets.key` (generated once, mode `0600`), so the
file never holds a secret in the clear. The API returns a secret as `set` plus a
four-character hint, never as its value; changes are audited on the
`rustybin::settings` target with the key and the principal (the `sub` of a control
JWT, the admin token, or `open`), never the value. `RUSTYBIN_SETTINGS_LOCK=true`
makes the environment authoritative for instances a platform or a configuration
repository manages. Reads and writes need the admin token or the `admin` scope:

```hurl
{{#include ../../examples/concepts/settings.hurl:admin}}
```

Everything the instance sends of its own goes through the [egress
policy](../configuration.md#outbound-connections): an allowlist, pinned addresses,
always-refused ranges and an audited decision per call.

## Readiness

`GET /_rustybin/ready` answers `200 {"status": "ready"}` whenever the process serves
HTTP, whatever the [`/health` toggle](../reference/reliability.md#health-toggle) says.
It needs no credentials, is never limited by a plan and is never captured by the
inspector, so it is the right target for an orchestrator's readiness or health check
(a presenter flipping `/health` to 503 must not get the instance restarted).

## Metrics

`GET /_rustybin/metrics` serves Prometheus text (version 0.0.4), protected like the
rest of the control plane (`console` scope or the admin token):

| Series | Labels |
|---|---|
| `rustybin_requests_total` | `route` (the matched route template such as `/status/{code}`, or `unmatched`), `method`, `status_class` (`2xx`...) |
| `rustybin_request_duration_seconds` (histogram) | `route`; time until response headers, buckets from 1 ms to 60 s (see below) |
| `rustybin_streams_open` (gauge) | open SSE responses and WebSocket connections |
| `rustybin_egress_bytes_total` | response body bytes |
| `rustybin_protocol_requests_total` | `protocol`: `http`, `graphql`, `grpc`, `websocket`, `sse`, `mcp`, `a2a`, `llm` |
| `rustybin_llm_requests_total` | mock LLM requests served: `provider` (`openai`, `azure`, `anthropic`, `gemini`, `bedrock`, `ollama`, `cohere`), `model_family` (as for tokens), `streaming` (`true`, `false`) |
| `rustybin_llm_tokens_total` | `provider`, `model_family` (a fixed list such as `gpt-4o`, `claude`, `gemini`; anything else is `other`), `direction` (`input`, `output`) |
| `rustybin_llm_faults_total` | faults injected on the mock LLM with `X-Rustybin-Fail` or `?fail=` (see [AI faults](../ai/gateway-features.md#faults)): `provider`, `kind` (`rate_limit`, `server_error`, `unavailable`, `overloaded`, `timeout`, `context_length`, `prompt_filter`, `content_filter`, `invalid_credential`, `missing_credential`, `forbidden`, `not_found`, `bad_request`, `too_large`, `status_4xx`, `status_5xx`) |
| `rustybin_limit_rejections_total` | requests rejected by the [plan limiter](plans-and-limits.md) (HTTP 429 or gRPC `RESOURCE_EXHAUSTED`): `dimension` (`rps`, `concurrency`, `streams`, `requests`, `egress`); all five are always exported |
| `rustybin_faults_injected_total` | `kind`: `fail`, `delay` (`X-Rustybin-Fail`, `X-Rustybin-Delay`), `ai` (mock LLM error responses) |
| `rustybin_mock_requests_total` | requests to [OpenAPI mocks](../mocks/index.md): `mock` (the mock name; not exported in public mode, at most 1024 series, then `other`), `outcome` (`mocked`, `not_found`, `method_not_allowed`, `unauthorized`, `forbidden`, `invalid_request`, `not_acceptable`, `disabled`, `not_deployed`) |
| `rustybin_mock_visits` (gauge) | running [self-service](../mocks/self-service.md) visits (only with self-service on) |
| `rustybin_mock_visit_events_total` | `event`: `started`, `extended`, `ended`, `email_required`, `daily_limit`, `time_used`, `too_early`, `busy`, `rate_limited`, `invalid_email`, `paused`, `not_found` |
| `rustybin_mock_leads_total` | email addresses captured by self-service visits, by `result`: `captured`, `delivered`, `retried`, `dropped` (never an address) |
| `rustybin_build_info` (always 1) | `version`, `git_sha` |

Every label is bounded: raw paths, query strings and model names never become label
values, so a scanner hitting random URLs cannot blow up the series count. Model names
map to a fixed list of families, providers and fault kinds come from fixed lists, and
every other value becomes `other`.

```hurl
{{#include ../../examples/concepts/control_auth.hurl:metrics}}
```

The mock LLM series after one completion and one injected `rate_limit` error:

```hurl
{{#include ../../examples/concepts/control.hurl:metrics_llm}}
```

### Latency percentiles

`rustybin_request_duration_seconds` has the buckets 0.001, 0.0025, 0.005, 0.01,
0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 30 and 60 seconds (plus `+Inf`), so
`histogram_quantile` gives usable values from a fast stub (about 1 ms) up to a slow
mock LLM stream or a long injected delay. No two neighbouring bounds are more than
3x apart, so an estimated percentile is within one bucket of the true value.

```promql
# p50 and p99 per route over the last 5 minutes
histogram_quantile(0.50, sum by (le, route) (rate(rustybin_request_duration_seconds_bucket[5m])))
histogram_quantile(0.99, sum by (le, route) (rate(rustybin_request_duration_seconds_bucket[5m])))

# p99 over every route
histogram_quantile(0.99, sum by (le) (rate(rustybin_request_duration_seconds_bucket[5m])))

# Plan limit rejections per second, by dimension
sum by (dimension) (rate(rustybin_limit_rejections_total[5m]))

# Share of streaming mock LLM requests
sum(rate(rustybin_llm_requests_total{streaming="true"}[5m]))
  / sum(rate(rustybin_llm_requests_total[5m]))
```

The histogram measures the time until response headers: for an SSE or streaming LLM
response that is the time to the first byte, not the length of the stream.

A Prometheus scrape job:

```yaml
scrape_configs:
  - job_name: rustybin
    metrics_path: /_rustybin/metrics
    authorization:
      credentials: change-me        # RUSTYBIN_ADMIN_TOKEN
    static_configs:
      - targets: ["rustybin.example.com:80"]
```

`GET /_rustybin/usage` also carries the build (`version`, `git_sha`, `profile`), the
instance id and the control auth mode, so one call tells a collector which version
runs with which plan:

```hurl
{{#include ../../examples/concepts/control_auth.hurl:usage_build}}
```

## Logs

`RUSTYBIN_LOG_FORMAT=json` writes one JSON object per line: `timestamp`, `level`,
`target`, `message` and the event fields at the top level, plus a `span` object with
the request's `method`, `path` and `request_id` (the `X-Request-Id` the response
carries), so log lines join with gateway logs on the request id:

```json
{"timestamp":"2026-10-05T12:00:00.000000Z","level":"INFO","message":"finished processing request","latency":"0 ms","status":200,"target":"tower_http::trace::on_response","span":{"method":"GET","path":"/uuid","request_id":"7d0c...","name":"request"}}
```

## Containers

The Docker image runs as an unprivileged user (uid and gid `10001`) and works with a
read-only root filesystem. Rustybin writes only three things: the demo PKI next to
`RUSTYBIN_TLS_CERT` (default `/app/certs`), the optional `RUSTYBIN_USAGE_FILE`, and
the optional `RUSTYBIN_STATE_DIR` (settings, secrets, saved mocks and scenarios).
Put them on a writable volume:

```bash
docker run -d --read-only --cap-drop ALL --security-opt no-new-privileges \
  -v rustybin-data:/app/certs \
  -e RUSTYBIN_USAGE_FILE=/app/certs/usage.json -e RUSTYBIN_STATE_DIR=/app/certs/state \
  -p 8080:80 -p 8443:443 -p 50051:50051 ghcr.io/coldbathcode/rustybin:latest
```

The named volume inherits the image's ownership at `/app/certs` (uid `10001`).
Keeping state in its `state/` subdirectory avoids an unwritable, root-owned
volume at a new mount path. A bind mount or separate state volume needs ownership
prepared for uid `10001`.

Without a writable `/app/certs` the demo certificates live in memory and change at
every restart (client certificates issued earlier stop verifying). Docker lets an
unprivileged process bind ports below 1024 inside the container
(`net.ipv4.ip_unprivileged_port_start=0`). On runtimes that do not (Kubernetes
without that sysctl, some micro-VM platforms), move the listeners up:
`RUSTYBIN_HTTP_PORT=8080 RUSTYBIN_HTTPS_PORT=8443`.
