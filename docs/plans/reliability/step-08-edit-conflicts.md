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

## Build notes (2026-11-02)

- The check is in the store, under its write lock: `MockStore::set_draft_if(name, draft,
  rename, base)` answers `StoreError::Stale { expected, current }` when the draft is not
  revision `base`. `set_operation` and `Svc::update` take an expected revision; without one
  they still compare-and-swap against the draft they read, and make the edit again on top of
  a change that raced it (up to three times), so concurrent edits are never lost. A store-wide
  `edit_lock` serialises the read, recompile and write of those two edits (they run on the
  blocking pool); a test races two threads editing different operations of one mock.
- `409` carries `rustybin_stale_revision`, both revisions and `changed`
  (`changes_between` of the revision the edit was made from, found in the live revision or
  the history, and the current draft; "no longer in the history" otherwise). Nothing is saved.
- `If-Match` takes `"12"`, `W/"12"` or `12`; with `expected_revision` too they must agree
  (else `400`). The mock's `GET`, the operation's `GET` and both edits answer `ETag: "<draft
  revision>"`; the operation answer gained `draft_revision`.
- `mocks.require_revision` (new settings group `mocks`, Management, General; no environment
  variable): an edit without a revision answers `428 rustybin_revision_required`.
- Console: `ui/lib/mockapi.js` remembers each mock's draft revision from every answer that
  carries one and sends `If-Match` on `PATCH /{name}` and `PUT /{name}/operations/{op}`; a
  `409` names what changed and offers to reload (Cancel keeps the form). The UI conformance
  edits a mock behind the console's back and checks the save is refused and nothing saved.
- CLI: `mocks update` and `mocks set-operation` send `--expect-revision N`, else the draft
  revision read just before the edit (so an instance with the setting on still accepts them).
  MCP: `expected_revision` on `update_mock` and `set_operation`, described as always to pass.
  The AI edit (`/{name}/ai/edit` with apply) saves with the revision the model edited, and now
  through `Svc::compiling` (it called `update` on the async runtime before).
- Also here (the docs job of step 07 failed): the new testing-workflow tools moved to the end
  of `TOOLS`, so the long-standing tools keep their place on the first `tools/list` page, and
  `docs/examples/agent/manage.hurl` follows `nextCursor` to the second page; the agent page
  says that `tools/list` pages at 100.
