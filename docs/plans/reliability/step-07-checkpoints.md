# Step 07: Checkpoints and reset

## Goal

Save the state a demo starts from and restore it before a run, with the scope stated: the
instance's mocks (definitions, deployed revisions, enabled, stateful data), the health toggle and
the fault settings (flaky counters reset). Never visitors' mocks, never settings or secrets.

## Model

`POST /_rustybin/checkpoints` (name), `GET`, `DELETE`, `POST /_rustybin/checkpoints/{name}/restore`
(admin, confirmed on the MCP). At most 20 checkpoints, kept in `<state>/checkpoints/`. A restore
applies the saved mocks through `MockStore::apply` with pruning limited to the instance's API mocks,
restores stateful data, sets health, resets the flaky counters, and answers what changed.

## Surfaces

API, console (Management, Checkpoints), management MCP, CLI (`rustybin checkpoint save | list |
restore | delete`), `rustybin.yaml`: nothing (a checkpoint is a snapshot of the instance, the
document already is one).

## Tests

Save, change mocks, stateful data and health, restore: the same export as before; repeatability
(restore twice, the same answer); a visitor's mock is untouched; settings unchanged.

## Build notes (2026-11-02)

- `src/checkpoints.rs`. The scope is `covered`: instance-owned mocks of origin `api` with no
  expiry (so a gateway scenario's scratch mock is out, as are visitors', keys', file mocks and
  the sample). A checkpoint keeps, per mock, the deployed revision's spec text and settings, the
  draft when it differs, on or off, and the stateful data (`MockState::copy`, collections with
  their items oldest first, an emptied collection included so it is not seeded again).
- The restore is not `MockStore::apply` (the plan's wording): `apply` deploys one revision per
  mock, and a checkpoint must bring back a draft on top of a deployed revision and the on/off
  state. Each mock is compared first: identical is `unchanged`, only the data differs is
  `data_restored` (the data alone is put back, the revisions and traffic stay), otherwise the
  mock is recreated through `MockStore::restore` and saved (`MockStore::persist`). API mocks the
  checkpoint does not hold are deleted. A second restore therefore answers `changed: false`.
  `?dry_run=true` answers the same lists without changing anything. The restore runs on the
  compile pool (`Svc::compiling`).
- Bounds: 20 checkpoints, 16 MiB each, 64 MiB together, in `<state>/checkpoints/<name>.json`
  (0600), loaded at startup within the same caps.
- Console: Management, Checkpoints (save, preview, restore confirmed, delete); the UI
  conformance flips health and restores it. MCP: `list_checkpoints`, `save_checkpoint`,
  `restore_checkpoint` (confirmed unless `dry_run`), `delete_checkpoint` (confirmed).
- The management server now has more tools than a `tools/list` page holds well past the first
  page: the agents tests, the control OAuth tests and the Python conformance (`all_tools` in
  `conformance/mcp/common.py`) follow `nextCursor` instead of reading one page.
- Tests: save, change data, a deploy, a new and a deleted mock, health, flaky counters and a
  setting, then a dry run (nothing changes), a restore (the export, the deployed sources, the
  data and health as saved; the setting not), a second restore (nothing to change) and a data-only
  restore; a visitor's mock untouched and never held; admin only, names, the cap, replace, delete
  and a restart.
