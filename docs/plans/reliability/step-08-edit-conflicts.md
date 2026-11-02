# Step 08: Conflict protection for edits

## Goal

Two people or agents editing a mock from the same baseline cannot silently overwrite each other.

## Model

Every mock answer carries its draft `revision` (it does already) and an `ETag`. `PATCH
/_rustybin/mocks/{name}` and `PUT /_rustybin/mocks/{name}/operations/{op}` accept `If-Match` (or
`expected_revision` in the body); a stale one answers `409` with the current revision and a
summary of what changed. The console, the CLI and the management MCP always send it; the console
offers reload and compare. `mocks.require_revision` (setting, off) makes it mandatory for every
caller.

## Surfaces

API, console, management MCP (`expected_revision` on `update_mock` and `set_operation`), CLI
(`--expect-revision`, sent automatically from the last read when editing in one command),
`rustybin.yaml`: the setting.

## Tests

Two edits from the same revision: the second is `409` naming the current revision; a matching
revision passes; without one, passes unless the setting is on.
