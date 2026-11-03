# Step 06: A compare view for a stale edit

## Goal

When the console's edit is refused as stale, show what the other change did, side by side with the
edit that was refused, and let the operator keep theirs (re-apply on the new revision) or take the
other.

## Model

- `GET /_rustybin/mocks/{name}/revisions/{revision}` answers a kept revision's spec and settings
  (the history already holds them); the console diffs that against the current draft with the
  existing diff view, per section (spec, responses, security, an operation).
- "Keep mine" sends the refused edit again with the current revision; "Take theirs" reloads.

## Surfaces

API (the revision route), console (the compare dialog), management MCP (`get_mock_revision`), CLI
(`mocks revision NAME N`), `rustybin.yaml`: nothing.

## Tests

The revision route (kept, gone from the history, a visitor's mock is 404 to others); the UI
conformance makes a stale edit and keeps it.

## Build notes (2026-11-03)

- Drafts and deployed revisions share one store-wide numbering (a deploy makes the draft
  itself live), so the `expected_revision` of a `409` is a revision number like any other.
  The history held only previously live revisions, though, and a draft replaced before a
  deploy was lost: the common stale edit (two edits of an undeployed draft) had no base to
  compare. The store's `Entry` now also keeps `earlier`, the drafts replaced before they
  were deployed (newest last, as many as `MockLimits.revisions`: 5, 2 in public mode),
  pushed by `keep_earlier_draft` wherever the draft is replaced (`set_draft_inner`, the
  rollback, an apply's `replace_deployed`) unless it is live or in the history.
  `MockStore::kept_revision(name, n)` finds one among the draft, live, history and
  earlier drafts. In memory only (not persisted): after a restart only the saved draft
  and live revision are kept. The `409`'s `changed` now uses it too, so it names the
  sections for an undeployed base instead of "no longer in the history".
- `GET /_rustybin/mocks/{name}/revisions/{revision}` (`Svc::revision`, shared by the REST
  handler and the `get_mock_revision` tool): `readable` first (another visitor's mock is
  `404`, a non-number is `400`), then the revision or `404` "no longer kept". It answers
  `draft`, `live`, `current_draft_revision`, `changes_to_draft` (`changes_between` of the
  revision and the draft), `spec` (`format`, `openapi`, `title`, `version`, `text` as
  uploaded), `operations` (`id`, `method`, `path`), `settings` (redacted unless
  `Caller::reveals`; `mcp` is always there, also when it is the default that
  `MockSettings` leaves out when serialised, so two revisions compare section by section)
  and `secrets_hidden`. No `ETag` (it is not the mock).
- MCP: `get_mock_revision` (`Right::Read`, read only) at the end of `TOOLS`; the spec text
  is returned as `spec.text`, as `get_mock_spec` returns its `spec` (no `untrusted_*`
  field there either). CLI: `rustybin mocks revision NAME N` (the noun is `mocks`): a
  summary for people (kind, what the draft changed since, the spec's title, the
  settings), the whole answer with `--json`, the spec text only with `--spec`.
- Console: `ui/lib/conflict.js`. `mockapi.api()` opens the compare dialog on a `409
  rustybin_stale_revision` of a `PATCH /{name}` or `PUT /{name}/operations/{op}` and
  awaits it, so the callers did not change: with Keep mine the call answers what the
  edit sent again with `If-Match: <current_revision>` answered (the caller then shows
  its usual "Saved" and refreshes); Close or Escape answers the `409` (the form stays);
  Take theirs reloads the page. A deploy's `409` keeps the old reload prompt. The
  dialog reads both revisions, splits each into sections (spec, server prefix,
  responses, security, mcp, one per operation; settings as JSON with sorted keys and
  without empty values, the spec pretty-printed when it is JSON) and makes "mine" by
  applying the refused body to the base (a section the PATCH names replaces it, a PUT
  sets one operation). Each changed section shows `diffView(base, theirs)` beside
  `diffView(base, mine)` with a badge (your edit, their change stays, changed on both
  sides: yours replaces theirs). When the base is no longer kept, each section shows the
  current draft changed into the edit. A second `409` on Keep mine opens a new compare
  view. Text only through `h()` (no `innerHTML`).
- The UI conformance: the copy mock's stale save now checks the dialog's sections and
  Close; the stale-edits step changes the MCP server and an operation through the API,
  saves the console's seed, checks the compare (screenshot `21-mock-conflict-compare`),
  keeps it with the keyboard and checks all three changes are in the draft, then a
  conflict on the same section and Take theirs, then the deploy prompt as before.
- `rustybin.yaml`: nothing (kept revisions are the store's memory, not configuration).
