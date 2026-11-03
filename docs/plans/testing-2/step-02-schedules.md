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

## Build notes (built 2026-11-03)

- **Where it lives**: `src/replay/schedule.rs` (`Schedule`, `Schedules`, the scheduler and
  the notice delivery), held as `Tests.schedules` (`state.saved_tests.schedules`), not a
  new `AppState` field; `<state>/test-schedules.json` (schedule, `saved_at`, status), in
  memory without a state directory. Two tasks in `server.rs`: `test schedules`
  (`scheduler_loop`, a tick a second) and `test schedule notices` (`notice_loop`).
- **Model**: `name`, `description`, `names` or `prefix` (neither: every saved test),
  exactly one of `every` (`5m` to `24h`, `s`/`m`/`h`) and `at` (`HH:MM` UTC), `enabled`
  (default true, always exported), `webhook`, `notify_recovery`. `every` is aligned to
  multiples of the interval since the Unix epoch, so times survive restarts without
  bookkeeping; missed slots collapse into one run, and a slot missed while the instance
  was down is not caught up (the next time is computed from now at load).
- **One run at a time**: a schedule has a `running` flag, and every run takes the saved
  tests' single-flight guard (`Tests::begin_run`): a due run that finds another run going
  stays due and is tried at the next tick. Run by hand (the route, the CLI, the tool) is
  `409` while a run goes or the harness sends load.
- **Skipped runs** are recorded in the schedule's status, not in the history (a history
  run needs a report): `skipped` counts them, `last_result: skipped` with `last_detail`,
  and the last ten outcomes (`recent`). The harness check is `Harness::load_running` (a
  run of its own, one being prepared, or a cluster run it leads); tests force it with
  `Schedules.force_load`. A schedule whose tests are all gone is skipped too.
- **The run** goes through `replay::run_and_keep(state, "schedule-<name>", body,
  "schedule:<name>", "scheduler")` with empty variables, so the runner reads
  `RUSTYBIN_TEST_*` only. Named tests that are gone are left out and named in
  `last_detail`.
- **Notices**: one per failing run (failed or could not run), one more on recovery with
  `notify_recovery`. The body has the schedule, the counts, the failing names (50 at
  most), `newly_failing`, `run_path` and `run_url` (with the new `notify.base_url`
  setting), never what a test saw. Egress purpose `notify`, no rule of its own (only
  `egress.allow`); a refusal is final (`refused`), a failed post is tried 4 times (15 s,
  60 s, 240 s apart, then `failed`), at most 50 waiting (`dropped` beyond), in memory only,
  dropped with their schedule. The bearer token is the new secret `notify.webhook_token`
  (`RUSTYBIN_NOTIFY_WEBHOOK_TOKEN`). A webhook URL takes no userinfo nor query
  (`egress::parse_url(.., false)`); a secret in its path cannot be detected and is
  exported with the document (documented: the token goes in the setting).
- **Surfaces**: a fourth route and tool beyond the plan, `POST
  /_rustybin/tests/schedules/{name}/run` and `run_test_schedule` (confirmed when its tests
  go through a gateway or change state, or it has a webhook): it makes the webhook
  testable on demand. `save_test_schedule` is confirmed only when it names a new webhook.
  The CLI verbs follow step 01's hyphenated form: `tests schedules`, `schedule-show`,
  `schedule-save`, `schedule-run` (exit 1 on a failed run), `schedule-delete`. The
  console's Saved tests view has a Schedules card (list with status, on/off toggle, run
  now, edit, delete with confirmation, the form); Management, Network has a Test
  schedule notices card for the `notify` settings group.
- **`rustybin.yaml`**: `test_schedules:` after `tests:`, export, diff, apply and prune like
  `tests:`; a pulled document (`ApplyOptions.untrusted`) may name a webhook only when the
  saved copy already names it. The snapshot pins a schedule with `at: 02:30`.
- **A test can no longer be named `schedules`** (the route would hide it).
- `check_egress`'s purpose enum gained `scenario`, `replay` and `notify` (it missed the
  first two).
