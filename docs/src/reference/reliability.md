# Reliability: flaky endpoints, health and chaos

Endpoints for retry, circuit breaker, health check and failover demos. For faults on
any route, see [Fault injection](../concepts/fault-injection.md).

## Flaky endpoints

| Route (any method) | Behaviour |
|---|---|
| `/flaky/{fail_rate}` | Fails with `503` for `fail_rate` percent of requests (0 to 100), at random |
| `/flaky/pattern/{pattern}` | Follows a pattern of `S` (success) and `F` (failure), up to 64 characters, repeating: `SSF` fails every third request |
| `/flaky/after/{n}` | Succeeds `n` times, then fails: a circuit breaker trips |
| `/flaky/recover/{n}` | Fails `n` times, then succeeds: a circuit breaker half-opens and closes |
| `GET /flaky/status` | The caller's counters |
| `POST /flaky/reset` | Reset the caller's counters (`?scope=all`: everyone's, admin-guarded) |

Counters are kept **per session and per route** (`after:3`, `pattern:SSF`, ...): the
session is `X-Rustybin-Session` or, without it, the client IP (see
[Sessions](../concepts/sessions.md)). Two clients never share a counter, so several
people can run the same demo at once. Counters are bounded (10000, 2000 in public
mode) and forgotten after an hour of inactivity.

Failures are `503` with `Retry-After: 1` and a JSON body. Every response carries
`X-Rustybin-Flaky: true` and `X-Rustybin-Request-Number` (the counter value), and
`/flaky/{fail_rate}` also `X-Rustybin-Fail-Rate`.

```hurl
{{#include ../../examples/reliability/flaky.hurl:flaky_rate}}
```

```hurl
{{#include ../../examples/reliability/flaky.hurl:pattern}}
```

```hurl
{{#include ../../examples/reliability/flaky.hurl:after}}
```

```hurl
{{#include ../../examples/reliability/flaky.hurl:recover}}
```

```hurl
{{#include ../../examples/reliability/flaky.hurl:status_reset}}
```

```hurl
{{#include ../../examples/reliability/flaky.hurl:reset_all}}
```

## Health toggle

`GET /health` returns `200` with `{"status": "healthy", ...}`, or `503` with
`"unhealthy"` after the instance was marked unhealthy. The toggles are
instance-global and therefore guarded by the
[admin token](../concepts/sessions.md#admin-token) when one is configured (and
refused in public mode without one):

| Route | Effect |
|---|---|
| `POST /health/unhealthy` | Mark unhealthy (the call itself answers `200`) |
| `POST /health/healthy` | Mark healthy |
| `POST /health/toggle` | Flip the state |

The gRPC `grpc.health.v1.Health` service follows the same state (`NOT_SERVING`
while unhealthy), so HTTP and gRPC active health checks fail over together.

```hurl
{{#include ../../examples/reliability/health.hurl:health}}
```

```hurl
{{#include ../../examples/reliability/health.hurl:unhealthy}}
```

```hurl
{{#include ../../examples/reliability/health.hurl:healthy}}
```

`GET /` always answers `200`: use it (not `/health`) for platform liveness checks
such as the Fly.io check in `fly.toml`, so toggling the demo health state does not
get the machine restarted.

## Chaos recipes

| Scenario | How |
|---|---|
| Retry on 503, then success | `/flaky/recover/2` per client, or `X-Rustybin-Fail: 503:30` on any route |
| Circuit breaker opens | `/flaky/after/5`, then keep calling |
| Upstream timeout | `X-Rustybin-Delay: 5000` (or `/delay/5s`) behind a gateway timeout of 2 s |
| Slow body after fast headers | `/drip?duration=10&numbytes=10` |
| Failover between replicas | two instances with different `RUSTYBIN_INSTANCE_ID`, `POST /health/unhealthy` on one, watch `/identity` |
| Rate limited upstream | `/status/429` (with `Retry-After`) or, for LLM traffic, `X-Rustybin-Fail: 429` on `/ai/*` |
| gRPC errors | `EchoService/Fail` with any status code and retry info |
