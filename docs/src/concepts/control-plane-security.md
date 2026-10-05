# Control-plane security, metrics and operations

By default the control plane (`/_rustybin/*`) and the [web console](../console.md) are
open, which is what you want on a laptop or in a lab. When an instance is reachable
by others (a team server, a shared cluster, a hosted offering that runs one instance
per customer), lock the control plane down while the data plane stays public:

```bash
RUSTYBIN_CONTROL_AUTH=token RUSTYBIN_ADMIN_TOKEN=change-me rustybin
```

Data-plane routes (everything outside `/_rustybin/`) are never affected: they are the
product your gateway calls.

## Modes

| `RUSTYBIN_CONTROL_AUTH` | Control plane (`/_rustybin/*`) accepts |
|---|---|
| `open` (default) | Anyone. Single mutations still follow the [admin token rules](sessions.md). |
| `token` | Only `RUSTYBIN_ADMIN_TOKEN`, as `Authorization: Bearer <token>` or `X-Rustybin-Admin-Token: <token>`. |
| `jwt` | The admin token, or an Ed25519 (EdDSA) JWT signed by the key in `RUSTYBIN_CONTROL_JWT_PUBLIC_KEY`. |

Always reachable without credentials, in every mode:

- `GET /_rustybin/ready`, the readiness probe (see below);
- the data plane, including `GET /` (liveness) and `/health` (the demo toggle);
- the console's static files under `/ui/`. They are the same open-source files for
  every instance and carry no instance data: a browser must be able to load them to
  pick up a token from the URL fragment, which never reaches the server. Everything
  the console shows comes from `/_rustybin/*` with the token.

Without a token the control plane answers `401` with a `WWW-Authenticate: Bearer`
challenge and a JSON body naming the reason (`missing_token`, `expired`,
`invalid_audience`, `invalid_signature`, `invalid_algorithm`, `not_yet_valid`,
`missing_claim`, `malformed`, `invalid_token`):

```hurl
{{#include ../../examples/concepts/control_auth.hurl:no_token}}
```

```hurl
{{#include ../../examples/concepts/control_auth.hurl:open_routes}}
```

Unauthenticated control-plane requests are rejected before [plan limits](plans-and-limits.md)
count them; authenticated ones are counted but never rejected by the limiter.
`token` mode without `RUSTYBIN_ADMIN_TOKEN` (or `jwt` mode with neither a key nor an
admin token) refuses every control-plane request and logs a warning at startup.

## Signed access tokens (`jwt` mode)

A portal, a CI job or a script that holds the private key issues short-lived tokens;
the instance only needs the public key. A token must have:

| Claim | Rule |
|---|---|
| header `alg` | `EdDSA` (Ed25519); any other algorithm is refused |
| `aud` | equal to `RUSTYBIN_CONTROL_JWT_AUDIENCE` (default: `RUSTYBIN_INSTANCE_ID`), so a token for one instance does not open another |
| `exp` | required; 30 seconds of clock skew are tolerated (also for `nbf` when present) |
| `scope` | optional: a space separated string or an array; absent means `console` |

| Scope | Grants |
|---|---|
| `inspector` | read-only `/_rustybin/requests*` (the [request inspector](inspector.md) and its live feed) |
| `console` | read-only `/_rustybin/*` (everything the console reads, metrics included) |
| `admin` | everything, including mutations; also accepted wherever the [admin token](sessions.md) is (health toggles, global resets) |

A mutation (any method other than GET, HEAD or OPTIONS) needs the admin token or the
`admin` scope; other scopes get `403` with `reason: insufficient_scope`.

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
([`_mint-control-jwt.sh`](https://github.com/oliwynn/rustybin/blob/main/docs/examples/concepts/_mint-control-jwt.sh)
`<key> <audience> [scope] [ttl]`, used by the documentation runner):

```bash
{{#include ../../examples/concepts/_mint-control-jwt.sh:mint}}
```

Any JWT library with EdDSA support works too (PyJWT with `cryptography`,
`jsonwebtoken` in Rust, `jose` in JavaScript).

### The console with a token

Open the console with the token in the URL fragment:
`https://rustybin.example.com/ui/#token=<jwt>` (or `#/traffic&token=<jwt>` to land
on a view). The console keeps the token in memory and in `sessionStorage` (this
browser tab only), removes it from the address bar at once, and sends it as
`Authorization: Bearer` on every control-plane call, including the live traffic
feed, which it reads with `fetch` rather than `EventSource` so the header can be
sent (no token in a query string). When the token is missing, refused or expired
(the console also watches the `exp` claim), it shows a sign-in screen where a token
can be pasted, with a link back to `RUSTYBIN_CONSOLE_BACKLINK` when one is set.
**Console settings, Sign out** forgets the token.

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
read-only root filesystem. Rustybin writes only two things: the demo PKI next to
`RUSTYBIN_TLS_CERT` (default `/app/certs`) and the optional `RUSTYBIN_USAGE_FILE`.
Put both on a writable volume:

```bash
docker run -d --read-only --cap-drop ALL --security-opt no-new-privileges \
  -v rustybin-certs:/app/certs \
  -e RUSTYBIN_USAGE_FILE=/app/certs/usage.json \
  -p 8080:80 -p 8443:443 -p 50051:50051 rustybin
```

Without a writable `/app/certs` the demo certificates live in memory and change at
every restart (client certificates issued earlier stop verifying). Docker lets an
unprivileged process bind ports below 1024 inside the container
(`net.ipv4.ip_unprivileged_port_start=0`). On runtimes that do not (Kubernetes
without that sysctl, some micro-VM platforms), move the listeners up:
`RUSTYBIN_HTTP_PORT=8080 RUSTYBIN_HTTPS_PORT=8443`.
