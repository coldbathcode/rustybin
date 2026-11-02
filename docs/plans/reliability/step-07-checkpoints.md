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
