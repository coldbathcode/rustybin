# Plans and limits

Rustybin can enforce plan limits itself, for hosted offerings: a shared free demo
instance (public mode, limits per session) and one dedicated instance per paying
workspace (limits for the whole instance). The limits are off by default: with
`RUSTYBIN_PLAN=none` and no `RUSTYBIN_LIMIT_*` variable the limiter is not even in the
middleware stack and responses carry no extra header.

Every rejection says clearly that it comes from Rustybin, so a 429 in a gateway demo
is never mistaken for the gateway's own rate limiting.

## Presets

`RUSTYBIN_PLAN` selects a preset:

| | `free` | `pro` | `team` | `enterprise` |
|---|---|---|---|---|
| Scope | per session | instance | instance | instance |
| Requests per second (burst) | 5 (20) | 50 (200) | 250 (1000) | unlimited |
| Requests in flight | 10 | 100 | 500 | unlimited |
| Open streams | 3 | 25 | 200 | unlimited |
| Stream lifetime | 5 min | 1 h | 4 h | unlimited |
| Request quota | 10,000 per day | 1,000,000 per month | 10,000,000 per month | unlimited |
| Egress quota | 1 GB per day | 10 GB per month | 100 GB per month | unlimited |

`enterprise` enforces nothing but reports its name (`X-Rustybin-Plan`,
`/_rustybin/usage`). `none` is the default and changes nothing at all.

## Overrides

Each dimension can be overridden on its own; `0` or `unlimited` turns it off:

| Variable | Dimension |
|---|---|
| `RUSTYBIN_LIMIT_RPS`, `RUSTYBIN_LIMIT_BURST` | Token bucket: refill rate per second and capacity (burst `0` = the rps value) |
| `RUSTYBIN_LIMIT_CONCURRENCY` | Requests in flight; a response counts until its body is completely sent |
| `RUSTYBIN_LIMIT_STREAMS` | Open SSE responses (`text/event-stream`, including MCP and A2A streams and streamed mock LLM replies) and WebSocket connections |
| `RUSTYBIN_LIMIT_STREAM_SECS` | Stream lifetime in seconds |
| `RUSTYBIN_LIMIT_REQUESTS` | Requests per period |
| `RUSTYBIN_LIMIT_EGRESS_MB` | Response body megabytes per period (1 MB = 1,000,000 bytes, `0.5` works) |
| `RUSTYBIN_LIMIT_PERIOD` | `day` or `month`, calendar periods in UTC |
| `RUSTYBIN_LIMIT_SCOPE` | `instance` or `session` |

Setting an override without a plan turns the limiter on with the plan name `custom`.
See [Configuration](../configuration.md#plan-limits) for the full variable list.

**Scope.** `instance` keeps one set of counters. `session` keeps one per
[session key](sessions.md): the `X-Rustybin-Session` header, else the client IP (with
`RUSTYBIN_TRUST_FORWARD=true` behind a proxy). Session scope is a fairness tool for a
shared demo instance, not abuse protection: a client can pick a new session header.
At most 10,000 session counter sets are kept; when full, idle sets are dropped first.

## Responses

A successful response carries the plan and, when a request quota is configured, the
requests left in the current period:

```hurl
{{#include ../../examples/concepts/plans.hurl:success_headers}}
```

A rejected request gets `429 Too Many Requests` with:

- `Retry-After` in seconds (1 for rate, concurrency and streams; the time until the
  period ends for a used up quota);
- the IETF RateLimit fields of
  [draft-ietf-httpapi-ratelimit-headers](https://datatracker.ietf.org/doc/draft-ietf-httpapi-ratelimit-headers/):
  `RateLimit-Policy` lists every configured policy (`"rps"`, `"concurrency"`,
  `"streams"`, `"requests"`, `"egress"`, with `q`, `w` and `qu` parameters) and
  `RateLimit` names the exhausted one with `r=0`;
- `X-Rustybin-Limit` with the dimension: `rps`, `concurrency`, `streams`, `requests`
  or `egress`, and `X-Rustybin-Plan`;
- a JSON body with `"error": "rustybin_plan_limit"`, the dimension, the plan and a
  message that starts with "This 429 comes from the Rustybin plan limit, not from your
  gateway."

```hurl
{{#include ../../examples/concepts/plans.hurl:rps}}
```

Rejections happen before the request inspector and fault injection, so a rejected
request is cheap and is not captured. When the CORS layer is on, these headers are
exposed to browser clients.

## Quotas

The request quota counts every admitted request (rejected ones are free). The egress
quota counts response body bytes as they are sent. An exhausted quota rejects new
requests until the period ends; a response already in flight is never cut, so usage
can end slightly above the quota.

```hurl
{{#include ../../examples/concepts/plans.hurl:quota}}
```

```hurl
{{#include ../../examples/concepts/plans.hurl:egress}}
```

WebSocket frames are not counted as egress (only HTTP response bodies are).

## Streams

An SSE response or a WebSocket connection takes a stream slot instead of a request
slot while it is open. At the lifetime limit an SSE response ends cleanly with a final
comment line (`: rustybin plan stream lifetime reached`), and a WebSocket connection
is closed with code 1008 and the reason `rustybin plan stream lifetime reached`.

```hurl
{{#include ../../examples/concepts/plans.hurl:stream_lifetime}}
```

## Usage

`GET /_rustybin/usage` reports the plan, the scope, the configured limits, the current
period (start, end, seconds until the reset), requests and egress used and remaining,
the available rate tokens, requests in flight and open streams. With session scope it
only reports the caller's own counters (like the request inspector in public mode,
nobody sees other sessions). The [web console](../console.md) shows the same data in a
"Plan and usage" card on the overview when a plan is active.

```hurl
{{#include ../../examples/concepts/plans.hurl:usage}}
```

## Exemptions and the admin token

Never limited, counted or decorated: `/` (the platform health check), the web console
(`/ui/*`), `GET /_rustybin/usage`, `GET /_rustybin/status` (which the console
polls every few seconds), `GET /_rustybin/ready` and `GET /_rustybin/metrics` (so
monitoring scrapes never use up a quota or show up as traffic in the usage counters). Other control plane routes count like any other
request, but a request with the [admin token](sessions.md) is never rejected on them,
so an administrator can always reach a busy instance.

```hurl
{{#include ../../examples/concepts/plans.hurl:exempt}}
```

```hurl
{{#include ../../examples/concepts/plans.hurl:admin_bypass}}
```

## Persistence

With `RUSTYBIN_USAGE_FILE=/data/usage.json` the instance quota counters (requests,
egress bytes, period start) are written to that JSON file every 30 seconds when they
changed, and once more on graceful shutdown after the connections drained. At startup
the file is read back when it belongs to the current period, so a machine that scales
to zero and restarts mid-month keeps its monthly quota. A file from an earlier period
is ignored (the quota starts from zero). Rate tokens, in-flight requests and streams
are not persisted, and neither are session counters.

```bash
RUSTYBIN_PLAN=pro RUSTYBIN_USAGE_FILE=/data/usage.json rustybin
```

## gRPC

On the gRPC listener the rate and request quota of the plan apply to
`rustybin.echo.v1.EchoService` calls (the session key comes from the
`x-rustybin-session` metadata, else the peer address). A rejected call fails with
`RESOURCE_EXHAUSTED`, the same explanation as the HTTP body and `retry-after`,
`x-rustybin-limit` and `x-rustybin-plan` metadata. Health and reflection are exempt;
concurrency, streams and egress are enforced on the HTTP listeners only.

## Metrics

Every rejection, HTTP 429 or gRPC `RESOURCE_EXHAUSTED`, increments
`rustybin_limit_rejections_total{dimension}` in the
[Prometheus metrics](control-plane-security.md#metrics), with `dimension` one of
`rps`, `concurrency`, `streams`, `requests` and `egress` (the `X-Rustybin-Limit`
value). All five series are exported from the start, at 0, so an alert on
`rate(rustybin_limit_rejections_total[5m]) > 0` needs no `absent()` guard.

```hurl
{{#include ../../examples/concepts/plans.hurl:metrics}}
```
