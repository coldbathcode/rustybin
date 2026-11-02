# Master plan: testing workflows 2

Planned 2026-11-02, after 0.3.0, from the first candidate under consideration in
`docs/src/roadmap.md` and the build notes of the reliability plan. Built only when the owner says
go, one step at a time, each pushed as it lands.

## 1. What this milestone is

0.3.0 made an instance a test bench: gateway scenarios, saved tests that replay in CI,
checkpoints, edits that cannot overwrite each other. This milestone makes those workflows run
without a person at the console: saved tests on a schedule with a report history and a failure
notice, more gateway scenarios (circuit breakers, request size limits, CORS, header
transformation), checkpoints kept in git next to `rustybin.yaml`, and a compare view when an edit
is refused as stale.

## 2. Principles

- **A failing test first**, as in the last two plans.
- **The five-surface rule**: the API (catalogue and OpenAPI), the console, the management MCP, the
  CLI and `rustybin.yaml` (or a written reason) in the same step, the parity tests green.
- **Bounded state, bounded work**: a schedule list, a report history and every outbound call have a
  cap and a lifetime; outbound calls go through `egress::Egress` under a `Purpose`.
- **Nothing reaches what it did not create**, as before: a scheduled run uses the operator's
  saved tests only; a scenario cleans up only its own resources.
- **Project hard rules**: no em or en dashes, no panics on request paths, vendor neutral.

## 3. What 0.3.0 left

| From | What |
|---|---|
| Saved tests (0.3 step 06) | runs are manual; the last report is gone with the answer |
| Gateway scenarios (0.3 step 05) | five outcomes; circuit breakers, size limits, CORS and header rules are common asks |
| Checkpoints (0.3 step 07) | kept in `<state>/checkpoints/` only; a team sharing a repository cannot share them |
| Edit conflicts (0.3 step 08) | a `409` names what changed and offers a reload; it does not show the difference |

## 4. Steps, order and status

The marker a session looks for is the first row that is not done.

| Step | Title | Needs | Status |
|---|---|---|---|
| [01](step-01-run-history.md) | A history of saved-test runs | | |
| [02](step-02-schedules.md) | Saved tests on a schedule, with a failure notice | 01 | |
| [03](step-03-scenarios-resilience.md) | Scenarios: circuit breakers and request size limits | | |
| [04](step-04-scenarios-http.md) | Scenarios: CORS and header transformation | | |
| [05](step-05-checkpoints-in-git.md) | Checkpoints in the configuration repository | | |
| [06](step-06-conflict-compare.md) | A compare view for a stale edit | | |
| [07](step-07-release.md) | Docs and release 0.4.0 | all | |

## 5. How a session works

As in the reliability plan: read this file, the step file and CLAUDE.md; tests first, then the
code, then the docs (a user page with its "On every surface" table and a Hurl example per route);
run fmt, clippy (also with `rustup run stable`, which CI uses), the full suite under a watchdog,
`scripts/check-no-dashes.sh`, the docs examples and, when the console changed, `conformance/ui`.
Add the step's build notes, set the row to done with the date, push.

## 6. Decisions for the owner

- **The failure notice** (step 02): a webhook (a new egress purpose, `notify`, no default) is the
  plan; an email needs a mail service the instance does not have. Say if a webhook is not enough.
- **Schedules on a cluster**: a schedule runs on the instance that holds it; several replicas
  would each run it. The plan documents this rather than electing a runner.
- **Checkpoints in git** (step 05) hold mock data, which may be test data a team does not want in a
  repository: they are written only on an explicit commit, never by the automatic mode.
