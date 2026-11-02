# Step 04: Instance diagnostics

## Goal

One answer to "is this instance healthy, and if not, what do I do": `GET /_rustybin/diagnostics`
(admin), `rustybin doctor`, the `diagnose_instance` tool and a Diagnostics tab under Management.
It is not readiness (`/_rustybin/ready`) and not the demo toggle (`/health`).

## Checks

Each check has an id, a status (`ok`, `warn`, `fail`), a message and a fix, never a credential:

| Id | What |
|---|---|
| `state_dir` | configured, exists, writable (a probe file written and removed): a configured directory that cannot be written is `fail`, none at all is `warn` |
| `secrets_key` | from the environment or the state directory, or process-only (`warn`: stored secrets die with the process) |
| `secrets_unreadable` | stored secrets sealed under another key |
| `settings_file` | the settings document loaded, or the error |
| `mock_load_errors` | the store's load errors (files, the config file, saved mocks) |
| `config_file` | `RUSTYBIN_CONFIG_FILE` loaded, or its problems |
| `listeners` | HTTP, HTTPS and gRPC bound, or the bind error (recorded at startup) |
| `egress_refusals` | outbound calls refused in the last hour, by purpose |
| `source_control` | the last git operation failed |
| `default_password` | the console still accepts the default password |
| `harness` | harness mode on with an engine error |
| `assist` | AI assist configured but not enabled (the reason) |

## Surfaces

API (catalogue, OpenAPI), console (Management, Diagnostics, with a fix per row), management MCP
(`diagnose_instance`, read), CLI (`rustybin doctor`: a table, exit 1 on any `fail`, `--json`),
`rustybin.yaml`: nothing to configure (the checks read state; the catalogue parity entry says so).

## Build notes (2026-11-02)

- `src/diagnostics.rs`: `report(state)` runs the twelve checks in a fixed order and answers
  the worst status; the handler runs it on the blocking pool (the state directory probe
  writes a file). `Diagnostics` in `AppState.diagnostics` keeps the startup notes (at most
  32): `server.rs` records HTTPS, gRPC on HTTP and gRPC listeners that did not start or
  stopped. HTTP needs no check: its bind failure stops the process.
- New accessors read the state the checks need: `Settings::load_error` (the settings
  document that did not load, so the store started empty), `Settings::unreadable_secrets`,
  `SourceControl::last_op`, `Harness::engine_problem`.
- Surfaces: the route (admin, catalogue, OpenAPI), `diagnose_instance` (read only,
  `Right::Admin`), `rustybin doctor` (a table; `--json`; exit 1 on a `fail`; a noun of its
  own), the Diagnostics tab under Management (one row per check with its fix, Run again),
  `rustybin.yaml` nothing (the checks read state). Docs: the troubleshooting page starts
  with it, with its "On every surface" table and `docs/examples/config/diagnostics.hurl`.
- Tests: an unwritable state directory fails, startup notes and load errors are reported,
  an unloadable settings document fails, the route is admin only; the console conformance
  matches the tab's rows to the API's report.
