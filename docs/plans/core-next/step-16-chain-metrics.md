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

## Build notes

Built 2026-11-12 as specified. Where the build is more exact than the plan, or differs:

- **What `ok` and `failed` are.** The plan named the two without defining them. `ok` is a call that
  was answered, whatever the status (the report's `upstream` plus `gateway`); `failed` is a call
  with no answer (the report's `transport` without `stopped`). A call ended under way by the kill
  switch or the run's end is counted under its reason (`kill_switch`, `not_live`), exactly as the
  report puts it in `refused`, and not also as `failed`. So per hop the series sum to `calls` plus
  `refused` minus `transport_kinds.stopped` (a stopped call is in both of the report's numbers and
  once in the series).
- **Fed from `ChainStats`.** `ChainStats::refuse` and `record` answer the `result` of what they
  counted and `chain::call_on` hands it to `Metrics::harness_chain_call` once the stats lock is let
  go; nothing else feeds the series, and `arrived` answers nothing. `call_on` gained a `&Metrics`
  parameter (its one caller, the correlation layer, passes `state.metrics`). `Chain::send` is
  untouched.
- **Bounded labels.** The series is a fixed table per run in `metrics::HarnessSeries` (3 hops by 8
  results), under the same run bound as the other harness series (`MAX_HARNESS_RUNS`). The result
  names are `metrics::HARNESS_CHAIN_RESULTS`; a unit test keeps them equal to `ok`, `failed` and
  `safety::ChainRefusal::ALL`, and a constant assertion in `chain.rs` keeps the depth equal to
  `safety::MAX_CHAIN_HOPS`. A hop or a result outside the table is dropped, never a label.
- **No zeros at all.** The plan asked for no series for a run without a chain. The build goes one
  further: a `(hop, result)` appears once it is counted, so a chain run exports only what happened
  (the docs say to use `sum(rate(...))`). The `# HELP` and `# TYPE` lines appear only when some run
  has a count.
- **The tests** are added to the existing chain tests instead of being new runs (the suite stays
  fast): every chain test that ends with a report now also compares the node's series with the
  node's own chain numbers (`series_agree`), the kill switch test reads the series before and after
  the switch, the cluster test reads each node's, and the loop-through test of `harness/tests.rs`
  asserts a run without a chain has no series. They were written with the code; each protection was
  then removed once to see them fail (the calls not fed, the refusals not fed, zeros exported, an
  arrival counted as a call).
- **Docs.** `docs/src/harness.md` (server-to-server chains) has the label table and a tested example
  (`harness_chain_metrics` in `docs/examples/harness/api.hurl`); `docs/src/configuration.md` names
  the series where the harness series are mentioned; the series table of
  `docs/src/concepts/control-plane-security.md` gained a `rustybin_harness_*` row (it had none).
  The OpenAPI description of `/_rustybin/metrics` names the series.

Not built: a latency histogram per hop (the plan leaves it out). With the cargo feature
`harness-http3` nothing changes: the feature's code is not in `call_on` or `ChainStats`.

## For the merge

Changelog (Unreleased, harness):

- Load test mode: `rustybin_harness_chain_calls_total{run, hop, result}` on `/_rustybin/metrics`,
  the onward calls of a run's server-to-server chain on each server node while the run is on.
  `result` is `ok` (answered), `failed` (no answer) or why no call was made (`not_prepared`,
  `not_live`, `kill_switch`, `seen_before`, `untracked`, `busy`); the numbers are the report's. A
  run without a chain has no such series.

Rule for CLAUDE.md (core next, harness chains): a chain's Prometheus series is fed only by what
`chain::ChainStats` counts (`refuse` and `record` answer the `result`, `call_on` passes it to
`Metrics::harness_chain_call`), so the series and the report cannot drift; its labels are a fixed
table (`metrics::HARNESS_CHAIN_RESULTS`, `MAX_HARNESS_CHAIN_HOPS`), a new `ChainRefusal` goes in
both, and an onward call's arrival is in no series.

Roadmap: the chain series candidate, if listed, is done.
