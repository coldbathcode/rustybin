# Gateway scenarios

A gateway scenario proves one gateway feature end to end. Pick the outcome, and
Rustybin prepares the upstream the test needs, tells you how to point the gateway at
it, sends test traffic through the gateway, and compares what the client got with
what reached the upstream. Ending the run removes what it created and nothing else.
No gateway product is assumed: the steps are written for any gateway. There are
scenarios for [HTTP gateways](#gateway-scenarios) (the table below), for
[AI gateways](#ai-gateway-scenarios) and for [MCP gateways](mcp/security.md#mcp-gateway-scenarios)
(tool poisoning, rug pulls, tool shadowing, result injection, oversized results and
exfiltration links, against this instance's MCP attack servers).

| Scenario | What a pass proves | Upstream the run prepares |
|---|---|---|
| `retries` | The gateway retries a request the upstream fails with `503`, and the client sees the success | `/flaky/recover/1` (fails each request's first attempt) |
| `rate_limiting` | The gateway answers `429` past the limit, and the requests over it never reach the upstream | a mock with `GET /items` |
| `credential_injection` | Clients send no credential; the gateway adds the upstream's own, and the upstream accepts every request | a mock that requires `X-Upstream-Key` with a key made for the run |
| `timeouts` | The gateway stops waiting for a slow upstream and answers a gateway error before the upstream would have | a mock whose `GET /slow` answers after 3 seconds |
| `caching` | Repeated GETs come from the gateway's cache: only the first reaches the upstream | a mock whose `GET /catalog` answers `Cache-Control: public, max-age=300` |
| `circuit_breaker` | The gateway stops forwarding to an upstream that keeps failing and answers on its own, then lets a trial request through after the half-open time | `/flaky/after/0` (answers `503` to every request) |
| `request_size_limit` | The gateway refuses a body over the limit with `413` before the upstream sees it, and forwards one under it | a mock with `POST /upload` (any body) |
| `cors` | The gateway answers a CORS preflight from an allowed origin (and not from another), and a request from the allowed origin gets `Access-Control-Allow-Origin` | a mock with `GET /cors` and `OPTIONS /cors` that answers no CORS of its own (unless you say the upstream answers CORS) |
| `header_transformation` | The gateway adds a request header on the way to the upstream, removes one the client sent, and adds a response header on the way back | a mock with `GET /headers` |

What to turn on at the gateway for the two resilience scenarios:

- **`circuit_breaker`**: a circuit breaker on the route (often called passive health
  checks or outlier detection on the upstream) that opens after a few failed requests
  in a row, answers on its own while open, and lets a trial request through after a
  half-open time. The check sends requests until one is answered without reaching the
  upstream (at most 20), waits the half-open time you give it (`half_open_ms`, 100 to
  60000, default 10000) plus a tenth, and sends up to three trials: a pass needs one
  of them to reach the upstream. The upstream keeps failing, so the trial fails and
  the circuit opens again: wait for it to close before checking again.
- **`request_size_limit`**: a request body size limit on the route. Give the check the
  limit in bytes (`size_limit`, 16 to 1048576, default 1048576): it sends
  `POST /upload` with a body half that size, then one twice that size (so a limit set
  in kilobytes or megabytes, either 1000 or 1024 to the unit, still falls between
  them). The body under the limit must also fit this instance's own
  `RUSTYBIN_BODY_LIMIT`.

What to turn on at the gateway for the two HTTP scenarios:

- **`cors`**: a CORS policy on the route that allows one origin and the `GET` method,
  allows no other origin, and answers preflight requests at the gateway. Give the check
  the origin you allowed (`allowed_origin`, default `https://app.example.com`) and one
  you did not (`refused_origin`, default `https://elsewhere.example.net`), each written
  as a browser sends it, `scheme://host[:port]`. The check sends an `OPTIONS` preflight
  (`Origin` and `Access-Control-Request-Method: GET`) from each origin, then a `GET`
  from the allowed one. A pass needs the allowed preflight answered `2xx` with
  `Access-Control-Allow-Origin` (that origin or `*`) and `Access-Control-Allow-Methods`
  with `GET`, without reaching the upstream; no `Access-Control-Allow-Origin` for the
  refused origin; and `Access-Control-Allow-Origin` on the `GET`. When the gateway
  should only let preflights through and the upstream answers CORS, check with
  `upstream_cors: true`: the run's mock then answers CORS for `allowed_origin`, and the
  allowed preflight must reach it. The run's mock never gets this instance's own CORS
  (`RUSTYBIN_CORS_ORIGINS`), so what the check sees comes from the gateway, or from the
  mock when you asked for it.
- **`header_transformation`**: request and response header rules on the route: add a
  request header to every request it forwards, remove one the client sends, and add a
  response header to every answer. Give the check the names you used
  (`add_request_header`, default `X-Gateway-Added`; `remove_request_header`, default
  `X-Client-Internal`; `add_response_header`, default `X-Gateway-Response`) and, when
  the value matters, `add_request_value`. The check sends a `GET /headers` with the
  header to remove (a value made for the check), then reads the
  [request inspector](concepts/inspector.md)'s capture of what reached the upstream and
  the answer. Name headers neither side sets on its own: `Host`, `Content-Type`,
  `Origin`, `X-Request-Id`, `X-Rustybin-*` and `Access-Control-*`, among others, are
  refused.

## AI gateway scenarios

The AI scenarios prove what an AI gateway does in front of a model. Their upstream is
this instance's [AI mock](ai/index.md) in the OpenAI chat completions format
(`/ai/openai/v1/chat/completions`, and a second target at `/ai/v1/chat/completions`,
the same API at another path, for the two scenarios that need two). A run steers the
mock for its own requests only: the check's requests carry a secret of the run in
`X-Rustybin-Session`, and the mock applies the run's profile (fail the primary target,
answer a customer record) to those and to nothing else, so other callers of the AI
mock see no change, and ending the run ends the profile. The check sends `POST`
requests in the OpenAI format (model `gpt-4o-mini`, `max_tokens` 100) to the URL you
give, so point it at the URL the gateway serves the route's chat completions at.

| Scenario | What a pass proves | What the mock does for the run |
|---|---|---|
| `ai_token_limit` | The gateway limits by tokens, not requests: once the budget is spent it answers `429`, the refused requests never reach the model, and the first refusal comes after about the budget | Answers each request with a little over 100 tokens of usage |
| `ai_model_fallback` | When the primary target answers a provider error, the gateway sends the request to a fallback and the client sees a success | The primary target answers every request with the error of `fail_kind`; the second target answers normally |
| `ai_semantic_cache` | Prompts that mean the same (other case and punctuation, a word more) come from the cache; a different prompt still reaches the model | Answers normally; its [embeddings](ai/embeddings.md) keep similar texts close, for a gateway that asks this instance for them |
| `ai_prompt_guard` | Personal data and a prompt injection never reach the model (the gateway refuses the prompt or redacts it), and a clean prompt does | Answers normally; the check reads what reached it |
| `ai_response_guard` | The gateway removes personal data from the model's answer before the client sees it | Answers every request with a fake customer record (SSN, card number, email, phone) |
| `ai_provider_balance` | The gateway spreads the requests across two targets, each getting at least its share, and every request succeeds | Two targets answer normally |

What each check sends and how it judges:

- **`ai_token_limit`**: the same long prompt until the gateway refuses one (at most 40
  requests, or until twice the budget went to the model). Give the check the budget you
  set (`token_limit`, 100 to 2000, default 500), counted in prompt plus completion
  tokens. The check reads `usage.total_tokens` from each answer; a pass needs a `429`,
  no `429` that reached the model, and the first `429` after the tokens that reached the
  model were between the budget less two requests' worth and a quarter over the budget
  plus two requests' worth (a gateway that counts completion tokens only, or checks a
  request's `max_tokens` up front, falls inside). A request limit set instead refuses
  too early and fails this line. Wait for the gateway's window to pass before checking
  again.
- **`ai_model_fallback`**: three requests. The primary target fails each with the
  provider error of `fail_kind` (`rate_limit` `429`, `overloaded` and `unavailable`
  `503`, `server_error` `500`, `timeout` `504`; default `rate_limit`). A pass needs every
  request answered `2xx`, its first upstream request to the primary target and failed,
  and a later one to another target answered `2xx`. Any other provider path of this
  instance counts as the fallback, for a gateway that falls back to another provider.
- **`ai_semantic_cache`**: a question, the same question in other case and
  punctuation, the same with a word more (`Please, ...`), then a different question. A
  pass needs the first and the different one to reach the model and neither of the
  other two. Set the gateway's similarity threshold at 0.9 or less. Each check asks
  another question (twelve in turn, from a point chosen per run), so what an earlier
  check left in the cache does not answer it. The gateway's own embedding calls, when
  they go to this instance with the session header, are not counted as model calls.
- **`ai_prompt_guard`**: a clean prompt, a prompt with fake personal data (SSN
  `123-45-6789`, card `4111 1111 1111 1111`, `jane.doe@example.com`) and, unless
  `skip_injection` is set, a prompt injection ("Ignore all previous instructions ...").
  The clean prompt must reach the model; each of the others must either be refused by
  the gateway or reach the model without what was planted. The verdict says which
  (refused, redacted, or the kinds that reached the model), never the values.
- **`ai_response_guard`**: one request whose answer from the mock is a fake customer
  record. A pass needs it to reach the model and the client's answer to hold none of
  the SSN, card number, email and phone: redacted, or the answer refused.
- **`ai_provider_balance`**: ten requests. A pass needs every one answered `2xx` and
  each target to have answered at least `min_share` percent of them (1 to 50, default
  20, which suits an even split and a weighted one down to 80/20).

```hurl
{{#include ../examples/scenarios/ai.hurl:ai_start}}
```

A check's parameters are refused before anything is sent when they do not fit:

```hurl
{{#include ../examples/scenarios/ai.hurl:ai_kind}}
```

The AI mock answers everyone else as before, while the run is on:

```hurl
{{#include ../examples/scenarios/ai.hurl:ai_untouched}}
```

Ending the run removes its profile:

```hurl
{{#include ../examples/scenarios/ai.hurl:ai_end}}
```

## How a run works

1. **Prepare.** Starting a run creates its upstream: a mock named `scn-<run>` (never
   saved, exported, kept in a checkpoint nor pruned; it ends with the run) or, for `retries` and `circuit_breaker`, a `/flaky` route. The answer has
   the upstream URL and the steps for the gateway.
2. **Configure the gateway.** Create a route whose upstream is that URL, turn on the
   feature, and let the route forward the `X-Rustybin-Session` request header. The
   check gives each of its requests its own session, so it can tell which upstream
   requests each one caused (gateways forward unknown headers by default).
3. **Check.** Give the URL the gateway serves the route at. Rustybin sends the
   scenario's requests there (plus the scenario's path, such as `/catalog`), reads
   the [request inspector](concepts/inspector.md) for what reached the upstream, and
   answers each expectation with what was expected and what was seen. Check as
   often as you like while you tune the gateway (at most 100 checks a run).
4. **End.** The run's mock is deleted and its flaky counters reset. Captured traffic
   stays in the inspector.

```hurl
{{#include ../examples/scenarios/scenarios.hurl:list}}
```

```hurl
{{#include ../examples/scenarios/scenarios.hurl:start}}
```

```hurl
{{#include ../examples/scenarios/scenarios.hurl:show}}
```

```hurl
{{#include ../examples/scenarios/scenarios.hurl:end}}
```

A verdict looks like this (a gateway without caching):

```json
{
  "scenario": "caching",
  "passed": false,
  "expectations": [
    { "name": "Every request got 200", "expected": "200 for 3 of 3", "seen": "200 for 3 of 3 (200, 200, 200)", "ok": true },
    { "name": "Only the first request reached the upstream", "expected": "1 upstream request", "seen": "3 upstream requests", "ok": false }
  ],
  "hints": []
}
```

When nothing reached the upstream, a hint says so: the route points elsewhere, or
the gateway drops the `X-Rustybin-Session` header.

## Reaching the gateway

Test traffic is outbound, so it goes through the [egress policy](concepts/control-plane-security.md)
under the purpose `scenario`, which has no default: add the gateway's host to
`RUSTYBIN_EGRESS_ALLOW` (and set `RUSTYBIN_EGRESS_ALLOW_LOOPBACK=true` for a gateway
on the same machine). Until then the check is refused with the reason:

```hurl
{{#include ../examples/scenarios/scenarios.hurl:check}}
```

The rate limiting check sends twice the limit you give it (`limit`, default 5, at
most 20); wait for the gateway's window to pass before checking again.

Each scenario lists its test parameters under `parameters` (name, type, bounds,
default), and a run repeats them: the console shows a field for each, the CLI has a
flag for each (`--limit`, `--half-open-ms`, `--size-limit`, `--allowed-origin`,
`--refused-origin`, `--upstream-cors`, `--add-request-header`, `--add-request-value`,
`--remove-request-header`, `--add-response-header`, `--token-limit`, `--fail-kind`,
`--skip-injection`, `--min-share`, `--max-result-kib`), the `check_gateway_scenario`
tool takes them by name.

| Parameter | Scenario | Type and bounds | Default |
|---|---|---|---|
| `limit` | `rate_limiting` | integer, 1 to 20 | 5 |
| `half_open_ms` | `circuit_breaker` | integer, 100 to 60000 | 10000 |
| `size_limit` | `request_size_limit` | integer, 16 to 1048576 | 1048576 |
| `allowed_origin` | `cors` | an origin `scheme://host[:port]`, http or https, at most 200 characters | `https://app.example.com` |
| `refused_origin` | `cors` | an origin, another than `allowed_origin` | `https://elsewhere.example.net` |
| `upstream_cors` | `cors` | `true` or `false` | `false` |
| `add_request_header` | `header_transformation` | a header name, at most 64 characters | `X-Gateway-Added` |
| `add_request_value` | `header_transformation` | visible characters, at most 200 (optional) | any value |
| `remove_request_header` | `header_transformation` | a header name, another than `add_request_header` | `X-Client-Internal` |
| `add_response_header` | `header_transformation` | a header name | `X-Gateway-Response` |
| `token_limit` | `ai_token_limit` | integer, 100 to 2000 | 500 |
| `fail_kind` | `ai_model_fallback` | one of `rate_limit`, `overloaded`, `server_error`, `unavailable`, `timeout` | `rate_limit` |
| `skip_injection` | `ai_prompt_guard` | `true` or `false` | `false` |
| `min_share` | `ai_provider_balance` | integer, 1 to 50 (percent) | 20 |
| `max_result_kib` | `mcp_oversized_result` | integer, 16 to 1024 (KiB) | 256 |

A parameter out of its bounds or of the wrong type, an unknown one, or one given to a
scenario that does not take it, is refused with the bounds before anything is sent:

```hurl
{{#include ../examples/scenarios/scenarios.hurl:parameters}}
```

```hurl
{{#include ../examples/scenarios/scenarios.hurl:origins}}
```

A verdict describes the headers the gateway answered (present, absent, the allowed
origin, `*`, another origin) rather than repeating their values.

## Who sees a run

Starting, checking and ending a run need the admin token (or a control token with
the admin scope): a run creates a mock and sends traffic. A run belongs to the
session that started it (`X-Rustybin-Session`, else the client address): another
session neither lists nor ends it. At most 32 runs exist at once, each for an hour.
A run id is 96 random bits, and the sessions of a check's requests carry a secret of
the run that is never shown, so requests sent to the upstream by anyone else never
count in a verdict. The `credential_injection` key is in the setup only for callers
with admin rights (who can read it on the run's mock anyway): someone without them who
shares the session key (one address behind a NAT, a console-scope token) sees the run
with `key_hidden: true` and a placeholder in place of the key.
Pass the CLI's `--session` to reach a run the console started.

## On every surface

| Surface | Where |
|---|---|
| API | `GET /_rustybin/scenarios`, `POST /_rustybin/scenarios/{id}/runs`, `GET`, `DELETE /_rustybin/scenarios/{id}/runs/{run}`, `POST .../check` |
| Console | **Gateway scenarios**: the HTTP, AI and MCP groups, prepare, the steps with the upstream URL (two for the scenarios with a second target), the gateway URL and the test parameters (a choice for `fail_kind`), **Check through the gateway**, the verdict table, **Clean up** |
| CLI | `rustybin scenario list`, `start`, `show`, `check`, `end` (`check` exits `1` when an expectation fails) ([reference](cli.md#scenario)) |
| Management MCP | `list_gateway_scenarios`, `start_gateway_scenario`, `get_gateway_scenario_run`, `check_gateway_scenario` (confirmed: it sends traffic out), `end_gateway_scenario` (confirmed) ([agent page](agent-mcp.md)) |
| `rustybin.yaml` | Nothing: runs are transient. A run's mock is never exported, saved to disk, kept in a checkpoint nor pruned; it ends with the run, and so does an AI run's profile |
