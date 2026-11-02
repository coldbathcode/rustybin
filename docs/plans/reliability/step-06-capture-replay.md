# Step 06: Capture, replay and assert

## Goal

A captured request becomes a saved test with assertions (status, headers, body, timing) that
replays directly against this instance or through a gateway, and runs in CI.

## Model

- A saved test: the request (method, path, headers, body; credentials redacted at capture are
  never filled back, the operator adds what is needed as `${VAR}` references), the target
  (`direct` or a gateway base URL), assertions (`status`, `header` equals or exists, `body_contains`,
  `json` path equals, `max_ms`). At most 200 tests, 64 KiB each, kept in `<state>/tests.json` and
  in `rustybin.yaml` (`tests:` section).
- `POST /_rustybin/tests` (from a capture id or a body), `GET`, `PUT`, `DELETE`, `POST
  /_rustybin/tests/run` (all or some, JSON or JUnit): direct replays go through the application in
  process with the caller's session; gateway replays through `Purpose::Replay` with a per-run cap
  (200 requests, 60 s).
- Export as Hurl (assertions included) for CI without a running console.

## Surfaces

API, console (Live traffic: "Save as test" on an entry; a Tests view with run and results),
management MCP (`save_test`, `list_tests`, `run_tests`; gateway runs confirmed), CLI (`rustybin
tests save | list | run [--junit] | export`), `rustybin.yaml` (`tests:`).

## Tests

Save from a capture, run direct (pass), change the mock, run again (the failing assertion named),
JUnit output, a gateway run refused by egress until allowed, redaction (a captured key is not in the
test), another session's captures cannot be saved.

## Build notes (2026-11-02)

- `src/replay/` (`mod.rs` model, store and routes; `run.rs` the runner and JUnit; `hurl.rs`
  the export). The routes are the plan's; `run` and `export` are reserved test names.
- **Variables are `{{name}}`, not `${VAR}`.** A `rustybin.yaml` loaded from a file has every
  `${VAR}` expanded at load (`config_file::expand_env`), which would turn a test's credential
  reference into the instance's own environment (or an error). `{{name}}` is also Hurl's own
  syntax, so the export needs no rewriting. A run reads its `variables`, else only the
  environment variables named `RUSTYBIN_TEST_<NAME>`: a test cannot read any other variable
  of the instance. MCP tools never take variables (secrets are never tool inputs).
- From a capture: the transport's and Rustybin's own headers are dropped; every
  secret-looking header and query value (`keys::looks_secret`) becomes a variable named
  after it, and the answer lists them (`introduced_variables`). A capture whose body was cut
  is refused. Visibility is the inspector's: a key's captures in shared mode, the session's
  on a public instance.
- Saving, replacing, deleting and running are admin; reading is the console scope. The MCP
  results go under `untrusted_*` (tests come from captured traffic; a run's `seen` values from
  answers); `run_tests` asks for confirmation only when a selected test has a gateway.
- `tests:` in `rustybin.yaml`: exported, diffed, applied (`apply_tests`) and loaded from
  `RUSTYBIN_CONFIG_FILE` (`Extra`). A test that fails validation is reported and kept, never
  pruned (the rule step 01 set for mocks). The export snapshot gains `tests: []`.
- Console: **Save as test** on a Live traffic entry (sends the entry's session), and the
  Saved tests view (run all or one, with variables for the run, failures, Hurl download,
  delete). The UI conformance covers both.
- Tests: from a capture to a passing run, the mock changed and the failing assertion named
  (JSON and JUnit), the Hurl export's lines; credentials become variables (and a run without
  them names the missing one); another session's capture on a public instance; a gateway run
  refused by egress, then allowed; validation and the cap; JSON paths; a restart; a document
  that prunes only what it does not name.
- Also here: the step 05 timeouts test delay is 1.5 s (600 ms failed once on a loaded machine).
