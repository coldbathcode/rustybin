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
