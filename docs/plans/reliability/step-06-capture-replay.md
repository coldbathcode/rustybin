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
