# Step 06: Customer demo packs

## Goal

Everything a customer's demo needs, in one bundle: mocks, portal items, saved tests, scenarios'
settings, storyboards and a console title. Applied with one command before the meeting, torn down
with one command after it, leaving the instance as it was.

## Where things stand

- `rustybin.yaml` holds the whole instance and can be applied (`src/configdoc/`), with a preview.
- Projects (`src/rbac/projects.rs`) group resources and count them (`project_api::resources`);
  deleting a project with `move_to` handles what it holds.
- Checkpoints snapshot instance-owned mocks.
- Nothing applies a document as a unit that can be removed again as a unit.

## Model

- A pack: a `rustybin.yaml` document restricted to the sections a demo needs (`mocks`, `portal`,
  `tests`, `test_schedules`, `storyboards`, `registry`), plus `pack: { name, title, customer, notes }`.
  Packs are files (a directory or a `.tar.gz` bundle, the existing `write_dir` and `tar_gz`), not a
  new store.
- Apply: creates a project named after the pack and applies the document into it (every resource it
  creates belongs to the project), after a preview that validates like the apply. Names that collide
  with existing resources are refused, never overwritten. The console title can be set for the
  duration (`console.title`, restored at teardown).
- Teardown: deletes the pack's project and everything it holds, restores what the pack changed in
  settings, and answers what was removed. A `dry_run` lists it first.
- Export: a project's resources as a pack (the inverse), so a demo built by hand becomes reusable.
- Built-in packs: none in this step; the docs carry one example pack in `docs/examples/packs/`.

## Surfaces

| Surface | What changes |
|---|---|
| API | `POST /_rustybin/packs/apply` (with `dry_run`), `GET /_rustybin/packs` (applied packs), `DELETE /_rustybin/packs/{name}` (teardown, with `dry_run`), `GET /_rustybin/packs/{name}/export`; permission `config.apply` and `users.manage` (projects are created and deleted under it today) plus what the document's sections need (the apply's own checks); whether a narrower permission for packs is wanted is the step's first question |
| Console | Management, Packs: upload or paste a pack, the preview, Apply; the applied packs with Teardown |
| Management MCP | `apply_pack`, `list_packs`, `teardown_pack` (destructive, `confirm`), `export_pack`; a prompt `prepare_customer_demo` |
| CLI | `rustybin pack apply <dir|file> [--dry-run]`, `pack list`, `pack teardown <name> [--dry-run]`, `pack export <project> <dir>` |
| `rustybin.yaml` | The `pack:` header; an instance export never contains it |

Docs: `docs/src/demo-packs.md`, the example pack, Hurl examples per route.

## Owner decision (master plan section 6)

A pack is a project. The alternative, prefixing every resource name with the pack's name, leaves
teardown to guessing by name and is not recommended.

## Rules

- A pack never carries a secret value (references only, as any document) nor `users`, `roles`,
  `sso.*`, `git.*` or `audit.*` (refused by section, like a document from git).
- A pack from an untrusted source is applied with `ApplyOptions.untrusted`.
- Teardown removes only what the pack's project holds; a resource moved out of the project by a
  person stays.

## Tests that fail first

- Apply then teardown leaves an export of the instance byte for byte what it was before.
- A pack naming an existing mock is refused at preview with the name.
- A pack with a `users:` section is refused.
- Export of a project, apply on a fresh instance: the same resources.

## Done when

A pack applies, tears down cleanly and exports on every surface, with the example pack tested in
`docs/examples/run.sh`.
