# Step 03: Git documents read whole

## Goal

Documents and specs read from the repository arrive intact up to their supported size, and only
diagnostic output is cut.

## Fixes

| Where | Problem | Fix |
|---|---|---|
| `src/sourcecontrol/mod.rs` `git` | every command's stdout is cut at 8 KiB and scrubbed, `extract` uses it for `git show` | a separate `git_bytes` for content reads: bounded by the document limit (8 MiB) and the spec limit, no truncation, no scrubbing; an oversize file is an error, never a cut file |
| `extract` | writes the cut text as the document | write the bytes from `git_bytes` |
| pull preview | `ok: true` on a truncated document | a read error is a problem that makes `ok` false |

## Tests

A 14 KB and a 2 MB spec through commit, push, a pull preview and an apply on a second instance:
no drift, the spec compiles, the preview is `ok`. A document over the limit is an error.

## Build notes (2026-11-02)

- `SourceControl::git_bytes(env, cwd, object, max)` runs `git show` with the same command
  line (`SourceControl::command`, now shared with `git`), keeps the bytes whole, and
  refuses more than `max`; `extract` reads the document (8 MiB) and each spec (the
  instance's `max_spec_bytes`, a new field) through it. A missing spec is still left to
  the apply to report; an oversize one fails the whole read.
- The test sends a 14 KB and a 900 KB spec through commit, push, a dry-run pull (which
  reads through `extract`) and an apply on a second instance: `ok`, no drift.
- Diagnostic output (`git`) keeps its 8 KiB cut and token scrub.
