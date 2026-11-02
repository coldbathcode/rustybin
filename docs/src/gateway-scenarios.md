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

## How a run works

1. **Prepare.** Starting a run creates its upstream: a mock named `scn-<run>` (never
   saved; it ends with the run) or, for `retries`, a `/flaky` route. The answer has
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

## Who sees a run

Starting, checking and ending a run need the admin token (or a control token with
the admin scope): a run creates a mock and sends traffic. A run belongs to the
session that started it (`X-Rustybin-Session`, else the client address): another
session neither lists nor ends it. At most 32 runs exist at once, each for an hour.
Pass the CLI's `--session` to reach a run the console started.

## On every surface

| Surface | Where |
|---|---|
| API | `GET /_rustybin/scenarios`, `POST /_rustybin/scenarios/{id}/runs`, `GET`, `DELETE /_rustybin/scenarios/{id}/runs/{run}`, `POST .../check` |
| Console | **Gateway scenarios**: prepare, the steps with the upstream URL, the gateway URL, **Check through the gateway**, the verdict table, **Clean up** |
| CLI | `rustybin scenario list | start | show | check | end` (`check` exits `1` when an expectation fails) ([reference](cli.md#scenario)) |
| Management MCP | `list_gateway_scenarios`, `start_gateway_scenario`, `get_gateway_scenario_run`, `check_gateway_scenario` (confirmed: it sends traffic out), `end_gateway_scenario` (confirmed) ([agent page](agent-mcp.md)) |
| `rustybin.yaml` | Nothing: runs are transient. While a run lasts its mock is an instance mock (an export includes it); it is never saved to disk and ends with the run |
