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
