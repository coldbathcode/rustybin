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

## Build notes (2026-11-02)

- `src/scenarios/catalog.rs` holds the five scenarios with their steps, request counts, upstream
  mocks and judgement; `src/scenarios/mod.rs` the runs and the routes. The plan's `{id}/runs`
  paths are kept (`/_rustybin/scenarios/{id}/runs/{run}` and `/check`), plus `GET` of a run
  (`rustybin scenario show`, `get_gateway_scenario_run`).
- What reached the upstream comes from the inspector, not from new state: every check request
  carries its own `X-Rustybin-Session` (`scn-<run>-<check>-<n>`), so the captures of that
  session are the upstream attempts it caused. The setup asks the gateway to forward that
  header; a check that sees nothing upstream says so in a hint. The `retries` upstream is
  `/flaky/recover/1`, whose per-session counter makes each request's first attempt fail.
- The upstream mocks are `MockStore::create_scratch`: instance-owned, deployed, never saved
  (a restart cannot orphan them), expiring with the run. Ending a run deletes the mock and
  resets the run's flaky counters (`FlakyState::reset_prefix`); captures stay.
- Runs belong to the session key (REST: the `Session` extractor; MCP: the same key from the
  request's headers and address). Starting, checking and ending are admin; reading is the
  console scope. A dropped check request clears the run's `checking` flag (a drop guard).
- The check answers statuses and timings only, never the gateway's headers or bodies, so the
  MCP result has no third-party content to put under `untrusted_*`.
- The timeouts upstream waits 3 s (1.5 s under `cfg(test)`: fast, with room for a loaded test machine).
- Tests: a stand-in gateway that retries, limits, adds a header, times out and caches passes
  every scenario; a pass-through proxy and the instance itself (no gateway) fail each with the
  right expectation named; ending removes the mock and the run's counters and nothing else (the
  sample and another session's counters stay; no saved copy); another session cannot see or
  end a run; admin and egress refusals; the run cap. The management tools now pass 100, the
  `tools/list` page size: the tests' `tool_names` follows the cursor.
