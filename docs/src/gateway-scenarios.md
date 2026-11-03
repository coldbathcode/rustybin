# Gateway scenarios

A gateway scenario proves one gateway feature end to end. Pick the outcome, and
Rustybin prepares the upstream the test needs, tells you how to point the gateway at
it, sends test traffic through the gateway, and compares what the client got with
what reached the upstream. Ending the run removes what it created and nothing else.
No gateway product is assumed: the steps are written for any gateway.

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
`--remove-request-header`, `--add-response-header`), the `check_gateway_scenario`
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
| Console | **Gateway scenarios**: prepare, the steps with the upstream URL, the gateway URL, **Check through the gateway**, the verdict table, **Clean up** |
| CLI | `rustybin scenario list`, `start`, `show`, `check`, `end` (`check` exits `1` when an expectation fails) ([reference](cli.md#scenario)) |
| Management MCP | `list_gateway_scenarios`, `start_gateway_scenario`, `get_gateway_scenario_run`, `check_gateway_scenario` (confirmed: it sends traffic out), `end_gateway_scenario` (confirmed) ([agent page](agent-mcp.md)) |
| `rustybin.yaml` | Nothing: runs are transient. A run's mock is never exported, saved to disk, kept in a checkpoint nor pruned; it ends with the run |
