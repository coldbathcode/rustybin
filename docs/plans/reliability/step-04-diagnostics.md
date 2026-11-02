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
