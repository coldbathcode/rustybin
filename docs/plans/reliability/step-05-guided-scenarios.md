# Step 05: Guided gateway scenarios

## Goal

Pick an outcome (retries, rate limiting, credential injection, timeouts, caching), and Rustybin
prepares what the test needs, says how to point the gateway at it, sends test traffic through the
gateway, shows what was expected against what reached it, and removes only what it created. No
gateway vendor is named anywhere.

## Model

- A static catalogue of scenarios (`src/scenarios/catalog.rs`): id, title, what it proves, the
  resources it creates (a mock with an operation in a given mode, a flaky route), the gateway setup
  in neutral words, the requests to send, and the expectations (status sequence, a header seen
  upstream, attempts per request, timing).
- A run: `POST /_rustybin/scenarios/{id}/runs` prepares (creates the resources, records their
  names), `POST .../runs/{run}/check` with the gateway URL sends the test traffic through the
  egress policy (`Purpose::Scenario`) and evaluates from the answers and the captures, `DELETE
  .../runs/{run}` removes what the run created. Runs are bounded (32, 1 hour) and owned by the
  caller (a visitor sees only theirs).

## Surfaces

API, console (a Scenarios view: pick, prepare, the setup text, check, the verdict table, clean
up), management MCP (`list_gateway_scenarios`, `start_gateway_scenario`, `check_gateway_scenario`,
`end_gateway_scenario`; the check is confirmed, it sends traffic out), CLI (`rustybin scenario list
| start | check | end`), `rustybin.yaml`: nothing (runs are transient).

## Tests

Each scenario against a loop-through "gateway" (the instance itself) and against a stand-in proxy
that behaves (passes) and one that does not (fails with the right expectation named); cleanup
removes the created mock and nothing else; another session cannot see or end the run.
