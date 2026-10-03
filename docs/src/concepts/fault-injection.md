# Fault injection

Two request headers turn any route into a misbehaving upstream, so retry, timeout,
circuit breaker and failover policies can be shown against the endpoint you were
already demoing. They work on every route except the control plane
(`/_rustybin/*`) and the web console (`/ui/*`).

| Header | Effect |
|---|---|
| `X-Rustybin-Delay: <ms>` | Wait that many milliseconds before handling the request (capped at 30000, 10000 in public mode). |
| `X-Rustybin-Fail: <status>` | Answer with that status (400 to 599) and a JSON error body instead of calling the route. |
| `X-Rustybin-Fail: <status>:<percent>` | The same with a probability: `503:50` fails about half of the requests (`503:50%` works too). |

Responses that were affected carry `X-Rustybin-Fault: fail` or
`X-Rustybin-Fault: delay`. Values that do not parse (a status outside 400 to 599, a
percent above 100, a negative delay) are ignored and the request is served
normally.

```hurl
{{#include ../../examples/concepts/faults.hurl:fail}}
```

```hurl
{{#include ../../examples/concepts/faults.hurl:fail_percent}}
```

```hurl
{{#include ../../examples/concepts/faults.hurl:delay}}
```

Both headers combine: the delay happens first, then the fault.

```hurl
{{#include ../../examples/concepts/faults.hurl:delay_and_fail}}
```

```hurl
{{#include ../../examples/concepts/faults.hurl:invalid_ignored}}
```

```hurl
{{#include ../../examples/concepts/faults.hurl:control_exempt}}
```

## On the mock LLM

On `/ai/*` the delay header works as above, but `X-Rustybin-Fail` is answered by the
mock LLM itself, in the provider's native error format (an OpenAI `rate_limit_exceeded`
body with `x-ratelimit-*` headers, an Anthropic 529 `overloaded_error`, a Gemini
`RESOURCE_EXHAUSTED`, ...), and accepts symbolic kinds such as `rate_limit`,
`overloaded`, `context_length` or `content_filter`. The mock LLM also has
`X-Rustybin-Latency-Ms`, `X-Rustybin-TTFT-Ms` and `X-Rustybin-Tokens-Per-Second`.
See [Faults, latency, credentials, inspection](../ai/gateway-features.md).

## Other chaos tools

| Tool | Use it for |
|---|---|
| [`/status/{code}`](../reference/http-basics.md#status-codes) | A fixed or weighted random status (`/status/200:0.9,503:0.1`) |
| [`/delay/{duration}`](../reference/http-basics.md#delays) | A slow endpoint (up to 60 s) without custom headers |
| [`/drip`](../reference/transfer.md#data-transfer) | A slow body after fast headers (streaming timeouts) |
| [`/flaky/*`](../reference/reliability.md) | Deterministic failure patterns per client: fail after n, recover after n, `SSF` patterns |
| [`/health/unhealthy`](../reference/reliability.md#health-toggle) | Active health checks and failover (the gRPC health service follows it) |
| gRPC [`Fail`](../reference/grpc.md) | Any gRPC status with rich error details |
| [`RUSTYBIN_REQUEST_TIMEOUT`](../configuration.md) | The server's own time-to-headers timeout (503) |
