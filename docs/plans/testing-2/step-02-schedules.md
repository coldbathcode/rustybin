# Step 02: Saved tests on a schedule, with a failure notice

## Goal

Run saved tests every N minutes (or at a time of day, UTC) without anyone at the console, keep the
report in the history, and tell someone when a run fails.

## Model

- A schedule: a name, the tests (all, names or a name prefix), `every` (`5m` to `24h`) or `at`
  (`HH:MM` UTC), on or off, a webhook URL for failures (optional), whether to notify on recovery.
  At most 20 schedules; in `rustybin.yaml` as `test_schedules:`.
- A scheduler task per instance; a run never overlaps the previous run of its schedule; a run that
  would start while the harness runs load is skipped and recorded as skipped.
- The webhook: a JSON summary (the schedule, the counts, the failing tests, a link to the run) under
  a new egress purpose `notify` (no default), with a bounded retry queue as the leads webhook has.
  Variables come from `RUSTYBIN_TEST_*` only (a schedule never stores a credential).

## Surfaces

API (`/_rustybin/tests/schedules*`), console (Saved tests: Schedules), management MCP
(`list_test_schedules`, `save_test_schedule`, `delete_test_schedule` confirmed), CLI (`rustybin tests
schedule list | save | delete`), `rustybin.yaml` (`test_schedules:`).

## Tests

A schedule runs on time (an injectable clock), never overlaps, records skipped runs; a failure sends
one webhook, a recovery one more when asked; the egress refusal is reported, never retried forever;
the document round trip.
