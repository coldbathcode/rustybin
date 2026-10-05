# Faults, latency, credentials, inspection

These request and response headers make the mock LLM useful behind an AI gateway:
they trigger the failures a gateway must handle, slow streams down, prove which
credential arrived and expose what the upstream received.

## Response headers

Every provider response (the `/ai/*` routes other than `/ai/requests`) carries:

| Header | Value |
|---|---|
| `X-Rustybin-Request-Id` | The request's `X-Request-Id` (generated when absent); the key for [request inspection](#request-inspection) |
| `X-Rustybin-Provider` | `openai`, `azure`, `anthropic`, `gemini`, `bedrock`, `ollama` or `cohere` |
| `X-Rustybin-Instance` | `RUSTYBIN_INSTANCE_ID`, for load balancing and failover demos |
| `X-Rustybin-Credential` | The credential the mock received, redacted to its last four characters (`bearer ****1234`), or `none` |
| `X-Rustybin-Model`, `X-Rustybin-Mode` | The model and [mode](behaviour.md#modes) that produced the reply |
| Rate-limit headers | On success: `x-ratelimit-*` (OpenAI, Azure) and `anthropic-ratelimit-*` (Anthropic), with the remaining tokens reduced by the request's usage |
| Provider request ids | `request-id: req_...` (Anthropic), `x-amzn-requestid` (Bedrock) |

## Faults

`X-Rustybin-Fail: <kind>[:<percent>]` answers with the provider's **native** error
(status, body shape and headers), always or with the given probability. The query
string works too, for clients that cannot set headers: `?fail=<kind>` and
`?fail_rate=` (a probability `0.3` or a percent `30`; `fail_rate` alone means
`503`).

| Kind | Result |
|---|---|
| `429`, `rate_limit` (`ratelimit`, `throttle`) | 429 with `Retry-After: 2` and exhausted rate-limit headers (OpenAI, Azure, Anthropic); Gemini `RESOURCE_EXHAUSTED`, Bedrock `ThrottlingException` |
| `529`, `overloaded` | Anthropic 529 `overloaded_error` (with `Retry-After`); 503 for the other providers |
| `500`, `server_error` | 500 |
| `503`, `unavailable` | 503 |
| `504`, `timeout` | 504 (Bedrock `ModelTimeoutException`) |
| `context_length` | 400 `context_length_exceeded` (or the provider's equivalent) |
| `prompt_filter` | 400, the prompt was blocked by a content filter |
| `content_filter` | **200** with the provider's filtered outcome (`finish_reason: content_filter`, empty content) |
| `401`, `auth` | The provider's invalid credential error (Gemini 400, Bedrock 403) |
| `403`, `404`, `400`, `bad_request`, `413` | That status in the native shape |
| any other 400 to 599 | That status |

Faulted responses carry `X-Rustybin-Fault: ai`. The `X-Rustybin-Delay` header
(see [Fault injection](../concepts/fault-injection.md)) also works on `/ai/*`.
Every fault that fires (`content_filter` included) is counted in
`rustybin_llm_faults_total{provider,kind}` and every served request in
`rustybin_llm_requests_total{provider,model_family,streaming}`, next to the token
counter (see [Metrics](../concepts/control-plane-security.md#metrics)).

```hurl
{{#include ../../examples/ai/faults_auth.hurl:rate_limit}}
```

```hurl
{{#include ../../examples/ai/faults_auth.hurl:overloaded}}
```

```hurl
{{#include ../../examples/ai/faults_auth.hurl:gemini_quota}}
```

```hurl
{{#include ../../examples/ai/faults_auth.hurl:context_length}}
```

```hurl
{{#include ../../examples/ai/faults_auth.hurl:content_filter}}
```

```hurl
{{#include ../../examples/ai/faults_auth.hurl:fail_rate}}
```

## Latency and streaming pace

| Header | Effect |
|---|---|
| `X-Rustybin-Latency-Ms: N` | Wait N ms before the response headers |
| `X-Rustybin-TTFT-Ms: N` | Time to first token: streams wait N ms after the headers before the first token; non-streaming responses add it to the latency |
| `X-Rustybin-Tokens-Per-Second: N` | Streaming pace, default 100; `0` streams as fast as possible |

Latency values are capped like `X-Rustybin-Delay` (30 s, 10 s in public mode) and
the pacing of one stream at 60 s (20 s in public mode). Use them for timeout,
time-to-first-token and streaming-throughput policies.

```hurl
{{#include ../../examples/ai/faults_auth.hurl:latency}}
```

```hurl
{{#include ../../examples/ai/faults_auth.hurl:pace}}
```

## Credential checks

Each provider looks for its own credential:

| Provider | Credential |
|---|---|
| OpenAI, Ollama, Cohere | `Authorization: Bearer <key>` |
| Azure OpenAI | `api-key: <key>` (or `Authorization: Bearer`) |
| Anthropic | `x-api-key: <key>` plus `anthropic-version` (or `Authorization: Bearer`) |
| Gemini | `x-goog-api-key: <key>` or `?key=<key>` (or `Authorization: Bearer`) |
| Bedrock | A SigV4 `Authorization: AWS4-HMAC-SHA256 Credential=<access key id>/<date>/<region>/bedrock/aws4_request, SignedHeaders=..., Signature=<hex>` plus `X-Amz-Date` (checked for structure only, never cryptographically), or a Bedrock API key as `Authorization: Bearer` |

The credential that was seen is **always** reported in `X-Rustybin-Credential`
(redacted). Checks are **enforced** only when asked for: per request with
`X-Rustybin-Require-Auth: true`, or for the whole instance with
`RUSTYBIN_AI_REQUIRE_AUTH=true` or `RUSTYBIN_AI_API_KEY=<key>` (then only that exact
key is valid; for SigV4 it is compared with the access key id). Failures use the
provider's native status and body (401; 403 for Gemini and Bedrock when the
credential is missing).

The classic demo: the client sends no key, the gateway injects the real one from
its vault, and `X-Rustybin-Credential` proves it arrived.

```hurl
{{#include ../../examples/ai/faults_auth.hurl:require_auth}}
```

```hurl
{{#include ../../examples/ai/faults_auth.hurl:native_credentials}}
```

## Request inspection

`GET /ai/requests/{id}` (the id from `X-Rustybin-Request-Id`) shows what the upstream
received: provider, endpoint, model, mode, stream flag, headers with credentials
redacted, the raw body, the **normalised prompt** (the same rendering as echo mode),
token usage, finish reason, a preview of the reply and, under `captured`, the
matching [request inspector](../concepts/inspector.md) entry (status, latency,
client IP). `GET /ai/requests` lists recent exchanges, newest first (`?limit=`,
default 50, at most 200; `?session=`).

Records are kept for an hour, at most 1000 (200 in public mode). In public mode
callers only see their own session (`X-Rustybin-Session` or the client IP).

```hurl
{{#include ../../examples/ai/faults_auth.hurl:inspect}}
```
