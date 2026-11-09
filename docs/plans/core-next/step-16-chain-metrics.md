# Step 16: A Prometheus series for chain calls

## Goal

Step 13's design named `rustybin_harness_chain_calls_total` and the build left it out: a chain's
numbers are in the report only. A dashboard that already scrapes `rustybin_harness_*` sees the
onward calls while the run is on.

## Model

- `rustybin_harness_chain_calls_total{run, hop, result}` on `/_rustybin/metrics`, a counter.
- `result` is a fixed set: `ok`, `failed`, and the refusals of `safety::ChainRefusal::ALL`
  (`not_prepared`, `not_live`, `kill_switch`, `seen_before`, `untracked`, `busy`).
- `hop` is at most `safety::MAX_CHAIN_HOPS`; `run` is bounded like the other harness series (the
  runs the harness keeps). Every label stays bounded, as `src/metrics.rs` requires.
- Fed from `chain::ChainStats`, the same numbers the report sums, so the series and the report agree
  at the end of a run. An onward call's arrival is never added to the clients' arrivals.
- A latency histogram per hop is not part of this step; the report has the legs.

## Surfaces

Metrics only. No route, tool, command or document section changes, so the five-surface rule has
nothing to apply to; the docs gain the series in `docs/src/harness.md` (server-to-server chains) and
wherever the harness series are listed.

## Tests that fail first

- A run with a chain: the series appears, and per hop `ok` plus `failed` plus the refusals equals
  what the report says.
- A run without a chain: no series, not even a zero.
- A cluster run: each server node exports its own calls; nothing is counted twice.
- The kill switch set mid-run: `kill_switch` counts rise and `ok` stops.

## Done when

The series is scraped during a chain run, matches the report, and is documented.
