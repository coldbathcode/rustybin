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

## Build notes (2026-11-12)

Built whole, with the recommendation of section 6: a pack is a project plus a document. Where the
build differs from the plan above:

- **There is a small record store after all** (`<state>/packs.json`, `AppState.packs`, 20 packs):
  the header, who applied it and when, and the two things the pack's project does not hold, which
  a teardown needs: the names of the pack's test schedules and the console title it replaced. The
  packs themselves stay files; the instance keeps no copy of the document.
- **`registry` is not a pack section.** The registry section of a document replaces the
  instance's file entries as a whole (`set_file_registry`), so it cannot be applied additively
  and removed again without touching what was there. A pack carries `mocks`, `portal`, `tests`,
  `test_schedules` and `storyboards`; everything else is refused by name.
- **What keeps a pack removable**: its portal items are items of its own mocks, its schedules
  name its own tests (no `prefix`, never every test), and a resource that names another project
  is refused. Schedules are not project resources, so they are the record's.
- **Specs are inline.** An instance reads no files: a pack sent to the API with `spec:` is
  refused, `rustybin pack apply <dir>` reads the spec files (paths inside the directory) and
  sends them inline. There is no `.tar.gz` pack: a pack with inline specs is one file, which is
  what `pack export` writes (`--output FILE`), so the plan's `pack export <project> <dir>` is
  `pack export <project> --output FILE`.
- **The apply is a unit** by a dry run first (`configdoc::apply` with `dry_run`, so the preview
  and the apply share every check), then the apply; if it still reports a problem, what it
  created is removed again and the answer is `422`. The portal section is left out of the dry
  run (its items name mocks the dry run has not created) and its metadata checked beside it.
- **The console title** is `pack.console_title`. The teardown restores the stored value from
  before, or unsets it when none was stored, and only when the title is still the pack's.
- **Permissions**: no new one (the step's first question). `config.apply` on the routes plus
  `users.manage` (the project), and `settings.write` with `settings.security` for a
  `console_title`; the list and the export are `config.export`.
- **The apply route takes the pack as the body** (`Content-Type: application/yaml`, with
  `?dry_run=true`), beside the JSON `{document, dry_run, untrusted}`, so `curl --data-binary
  @pack.yaml` and the docs' Hurl example send the example pack as it is.
- **Export** of a project that is not a pack takes the schedules that name only its tests, and
  the project's description as the title. A project's checkpoints are not in a pack (a pack
  creates state, a checkpoint snapshots it); a teardown does remove them.
- `untrusted` is the applier's word on the request (`ApplyOptions.untrusted`), since a pack has
  no source the instance could judge.

### For the merge

`src/packs/` (`mod.rs`, `tests.rs`), `AppState.packs`, the `pack` property of the configuration
schema, two rows of `BODY_CHECKS` and four of `NEW_SINCE_0_4`, four tools at the end of `TOOLS`
(the frozen rights, 162 tools for an admin) and the prompt `prepare_customer_demo` (eleven
prompts), the `pack` noun of the CLI, `ui/views/manage/packs.js` and its entry in `TABS`,
`docs/examples/packs/`.
