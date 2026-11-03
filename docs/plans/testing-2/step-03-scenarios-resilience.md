# Step 03: Scenarios: circuit breakers and request size limits

## Goal

Two more outcomes in the gateway scenario catalogue (`src/scenarios/catalog.rs`): a circuit
breaker opens after failures and closes again, and a request body over the limit is refused before
the upstream sees it.

## Model

- `circuit_breaker`: the upstream is `/flaky/after/N` per session (then always 503); the check sends
  requests until the gateway answers on its own (no new upstream attempt), waits the configured
  half-open time (a check parameter), and sees one trial request reach the upstream.
- `request_size_limit`: a mock accepting `POST /upload`; the check sends a body under and a body over
  the limit given as a parameter; expected: the small one reaches the upstream, the large one is
  answered `413` by the gateway and never captured upstream.
- Both keep the scenario rules: one session per request, nothing reaches what the run did not create,
  test parameters bounded.

## Surfaces

The scenario catalogue is one table: the API, console, CLI and MCP list them without new routes;
`rustybin.yaml` nothing. Docs: two rows in `docs/src/gateway-scenarios.md` with their setup words.

## Tests

The stand-in gateway of the scenario tests grows a breaker and a size limit; each scenario passes
through it, and fails with the right expectation named through a pass-through proxy and the
instance itself.

## Build notes (2026-11-03)

- **The breaker's upstream is `/flaky/after/0`.** Every request of a check carries its own
  `X-Rustybin-Session` (so its upstream captures are its own), and `/flaky` counters are per
  session, so any `N` above 0 would answer 200 to every request; with 0 it answers 503 to every
  one. The trial therefore fails too and the circuit opens again: the check proves "opens, then
  half-opens", not "closes"; the docs say to wait for the gateway before checking again. No new
  route was needed.
- **How the breaker check runs**: GETs until one is answered without an upstream capture (the
  capture is looked for five times, 50 ms apart, before the request counts as answered by the
  gateway), at most `MAX_BREAKER_REQUESTS` (20); a gateway that answers the very first request
  on its own fails (the route does not reach the upstream). Then it sleeps `half_open_ms` plus a
  tenth plus 100 ms from that answer and sends up to three trials 250 ms apart, stopping at the
  first that reaches the upstream. `Sent` gained `trial` and `bytes` (both omitted when unset).
- **The size check** POSTs `application/octet-stream` bodies of `size_limit / 2` and
  `size_limit * 2` bytes (a limit in KB or MB, 1000 or 1024 to the unit, still falls between
  them) to the run's mock (`POST /upload`, a binary request body). `size_limit` is 16 to 1 MiB
  (default 1 MiB), and refused when half of it exceeds `RUSTYBIN_BODY_LIMIT` (the body under the
  limit would not reach the upstream). Without a gateway limit the instance's own body limit may
  answer the large body 413, but that answer is captured, so "never reached the upstream" fails
  as it should.
- **Test parameters are a table now**: `catalog::Param` (name, label, description, bounds,
  default), `Scenario.params` per row, `params_of` validating (out of bounds, or a parameter of
  another scenario, is a `400` with the bounds, before the check counts or sends anything),
  `param_properties()` feeding the OpenAPI body schema and the `check_gateway_scenario` tool
  schema. The catalogue entry and the run carry `parameters` (also `[]` for scenarios without
  any; `rate_limiting`'s entry gained `label`, bounds and `default`). The console renders a
  field per parameter from the run (`#limit-<run>`, `#half-open-ms-<run>`,
  `#size-limit-<run>`); the CLI has `--half-open-ms` and `--size-limit`. `evaluate` takes
  `&Params` instead of the limit.
- **Docs**: two rows and a paragraph of gateway setup in `gateway-scenarios.md`, the parameters
  section with a Hurl example (`scenarios.hurl:parameters`, an out-of-range `size_limit`
  refused), the catalogue count 7 in the Hurl file and `conformance/ui` (which also prepares a
  circuit breaker run and checks its half-open field and the bounds message).
