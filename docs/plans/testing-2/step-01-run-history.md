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
