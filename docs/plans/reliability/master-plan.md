# Master plan: instance reliability and testing workflows

Planned 2026-11-02 from section 8 of the [hardening plan](../hardening/master-plan.md), once all
ten hardening steps were built. The three correctness findings of section 8.1 were re-run on
`5e94f7c` first: all three still reproduce (section 3).

## 1. What this milestone is

Two parts. First the correctness work section 8.2 asked for before any feature: an apply that
cannot load a definition never deletes what it names, previews validate exactly like the apply,
and git reads documents whole. Then the testing workflows of section 8.3: an instance that can say
what is wrong with it, gateway scenarios that set themselves up and check the result, captured
requests turned into tests that replay in CI, checkpoints a demo resets to, and edits that cannot
silently overwrite each other.

## 2. Principles

- **A failing test first**, as in the hardening plan.
- **The five-surface rule**: a capability lands on the API (catalogue and OpenAPI), the console,
  the management MCP, the CLI and `rustybin.yaml` (or a written reason it has no place there) in
  the same step, with the parity tests green.
- **Bounded state, bounded work**: every new store has a capacity and a lifetime; every outbound
  call goes through `egress::Egress` under a `Purpose`; replay and scenario traffic is capped.
- **Nothing reaches what it did not create**: scenarios clean up only their own resources,
  checkpoints never touch visitors' mocks or secrets, replays respect sessions and owners.
- **Project hard rules**: no em or en dashes, no panics on request paths, vendor neutral.

## 3. Re-run of the section 8.1 findings (2026-11-02, `5e94f7c`)

| Finding | Result |
|---|---|
| A failed apply prunes a live mock | Reproduces: `apply_mocks` drops a definition that fails to load before `MockStore::apply` prunes, while the dry run counts the name as kept (preview and execution disagree). |
| Concurrent mock mutations race during persistence | Fixed by hardening step 07 (`MockStore::persist_now`, `write_private`); step 01 adds the restart check section 8.1 asked for. |
| Dry runs accept what the apply rejects | Reproduces: `apply_settings` returns before validating; mock previews skip compilation. |
| Git previews truncate larger specs | Reproduces: `git show` output passes through the 8 KiB `MAX_OUTPUT` cut. |
| Git keeps the checked destination | Fixed by hardening step 06 (pinned `curloptResolve`, no redirects). |
| The assist catalogue was incomplete | Fixed by hardening step 01; step 09 checks it against section 8.3 row 6. |

## 4. Steps, order and status

The marker a session looks for is the first row that is not done.

| Step | Title | Needs | Status |
|---|---|---|---|
| [01](step-01-safe-pruning.md) | Safe pruning when a definition cannot load | | done 2026-11-02 |
| [02](step-02-validated-previews.md) | One validated plan for preview and apply | 01 | done 2026-11-02 |
| [03](step-03-intact-git-reads.md) | Git documents read whole | | done 2026-11-02 |
| [04](step-04-diagnostics.md) | Instance diagnostics: `rustybin doctor` and a health panel | | done 2026-11-02 |
| [05](step-05-guided-scenarios.md) | Guided gateway scenarios | 04 | done 2026-11-02 |
| [06](step-06-capture-replay.md) | Capture, replay and assert | | |
| [07](step-07-checkpoints.md) | Checkpoints and reset | 02 | |
| [08](step-08-edit-conflicts.md) | Conflict protection for edits | | |
| [09](step-09-ai-models-follow-through.md) | AI model selection follow-through | | |
| [10](step-10-release.md) | Docs and release | all | |

## 5. How a session works

As in the hardening plan: read this file, the step file and CLAUDE.md; tests first, then the code,
then the docs (a user page with its "On every surface" table and a Hurl example per route); run
fmt, clippy (also with `rustup run stable`, which CI uses), the full suite under a watchdog,
`scripts/check-no-dashes.sh`, the docs examples and, when the console changed, `conformance/ui`.
Add the step's build notes, set the row to done with the date, push.

## 6. Decisions for the owner

- **Release versions.** Section 8.3 suggests a first release with the correctness fixes and
  diagnostics. This plan ships one release at the end (step 10, `0.3.0`, since it adds features);
  cutting `0.2.3` after step 04 instead is a one-line change to step 10.
- **Expected revisions on edits** (step 08): the console, the CLI and the management MCP always
  send one; plain REST callers that send none keep today's behaviour unless the instance sets
  `mocks.require_revision`.
