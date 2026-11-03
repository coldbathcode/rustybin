# Step 01: A history of saved-test runs

## Goal

Every run of saved tests (console, CLI, MCP, and later the schedule) is kept: when, who or what
started it, which tests, the counts, each test's result and failures. A run can be read again,
downloaded as JUnit, and compared with the previous one (newly failing, newly passing).

## Model

- `<state>/test-runs/`: one JSON file per run, at most 200 runs and 30 days, oldest first out;
  in memory without a state directory (at most 50).
- `GET /_rustybin/tests/runs` (paged, newest first), `GET /_rustybin/tests/runs/{id}` (`?format=junit`),
  `DELETE /_rustybin/tests/runs` (admin). `POST /_rustybin/tests/run` answers the run id.
- A run's `trigger`: `console`, `cli`, `mcp`, `schedule:<name>`; the caller's principal, never a
  credential. Variables given to a run are never stored.

## Surfaces

API, console (Saved tests: a History panel, a run's detail, the change since the previous run),
management MCP (`list_test_runs`, `get_test_run`), CLI (`rustybin tests runs`, `tests run-show <id>
[--junit]`), `rustybin.yaml`: nothing (history is not configuration).

## Tests

Runs are kept and capped (count and age), a restart reads them back, the change against the
previous run names a newly failing test, a run's stored copy has no variable values.

## Build notes (built 2026-11-03)

- **Where it lives**: `src/replay/history.rs` (`History`, `TestRun`, `Change`), held as
  `Tests.history` (`state.saved_tests.history`), not a new `AppState` field. Every surface runs
  through `replay::run_and_keep(state, session, body, trigger, principal)`, which a schedule
  (step 02) calls with `schedule:<name>`.
- **Memory**: with a state directory only each run's summary (counts, change, each test's name
  and pass) is in memory; `GET .../runs/{id}` reads the run's file. Without one, the whole run is
  kept (50). A file that cannot be written keeps the run in memory with a warning: a run is never
  refused for its history. Writes and deletes go through `off_runtime`.
- **Ids**: `20261103T101500123Z-ab12cd` (UTC time to the millisecond, six hex digits): they sort
  by time, the age of a run read back is its id's time, and `valid_id` (letters, digits, `-`)
  keeps a path out of the file name. The age is pruned at every record and every list.
- **Paging**: `limit` (20, at most 100) and `cursor` (the `next_cursor` of the page before, the
  id of its last run), the shape of the registry's cursor; the answer also has `total`,
  `max_runs`, `max_age_days`.
- **The change**: against the most recent run with the same set of tests, else the most recent
  run; `newly_failing` is pass there and fail or error now, `newly_passing` the other way; tests
  absent from that run are neither. It is computed when the run is kept and stored with it.
- **The trigger** is a label: `X-Rustybin-Client: console` (the console sends it on its run
  call) or `cli` (the CLI's client sends it on every request), `mcp` for the management MCP,
  `api` otherwise. The principal is `settings::principal` (`jwt:<sub>`, `admin-token`, `open`) or
  `key:<id>` for a shared-mode control key on REST, `Access::principal` on the MCP.
- **Variables**: a run's given variable values (non-empty) are scrubbed to `{{NAME}}` from every
  result, in the answer and the kept copy, on top of the runner's own scrub of the variables a
  test uses; the run's `variables` are never part of `TestRun`.
- **Answers**: `POST /_rustybin/tests/run` keeps its fields and adds `run_id`, `trigger`,
  `principal`, `selection`, `change`; with `?format=junit` the id is in the
  `X-Rustybin-Test-Run` header. `run_tests` (MCP) answers the same plus the change in its summary.
- **Surfaces**: `DELETE /_rustybin/tests/runs` got a tool (`clear_test_runs`, confirmed) and a
  command (`tests runs-clear`, destructive) rather than a COVERAGE reason. The CLI verbs are
  `tests runs`, `tests run-show <id> [--junit]`, `tests runs-clear`.
- **A test can no longer be named `runs`** (the route `/_rustybin/tests/runs` would hide it); an
  existing test of that name is skipped with a warning at load.
- **Console**: a History card under Tests on the Saved tests view (the list with Older runs, a
  run's detail with every result, JUnit download, Clear history); the run's report shows the
  change. `conformance/ui/console_e2e.py` checks a console run is in the history with its detail
  and JUnit download.
