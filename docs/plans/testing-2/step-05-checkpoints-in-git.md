# Step 05: Checkpoints in the configuration repository

## Goal

A team that keeps its instance in git (0.2, step 09) keeps its demo checkpoints there too: a
checkpoint is written next to `rustybin.yaml`, committed with it, and restored on another instance
after a pull.

## Model

- `<repo>/<path>/checkpoints/<name>.json`, the same format as `<state>/checkpoints/`.
- `POST /_rustybin/git/commit` takes `checkpoints: [names]` (default none); a pull lists the
  repository's checkpoints and `POST /_rustybin/checkpoints/{name}/restore?from=git` restores one.
- Never written by the automatic mode; never a secret (a checkpoint has none; mock demo
  credentials are part of mocks and follow the export's `secrets` mode).
- A checkpoint read from git is untrusted input: size-capped, validated, compiled on the pool.

## Surfaces

API, console (Checkpoints: "in the repository"; Source control: the commit dialog's checkpoint
choice), management MCP (`git_commit` gains `checkpoints`; `restore_checkpoint` gains `from`), CLI
(`git commit --checkpoint NAME`, `checkpoint restore NAME --from-git`), `rustybin.yaml`: nothing.

## Tests

Commit with a checkpoint, pull on a second instance, restore: the same mocks and data; a
checkpoint over the size cap from git is refused; the automatic mode never writes one.

## Build notes (built 2026-11-03)

- **Where it lives**: `src/sourcecontrol/checkpoints.rs` (the git side: `to_write`, the
  working-copy write, `ls-tree` listing, the size-checked reads through `git_bytes`),
  and in `src/checkpoints.rs` `parse_untrusted` (the checks), `restore_checkpoint` (the
  local restore, split so a checkpoint from anywhere goes through it), `list_from_git`
  and `restore_from_git` (parse, checks and restore on `Svc::compiling`, like a local
  restore). No new `AppState` field, no new route: `?from=git` on the list and restore
  routes, `checkpoints` on the commit body.
- **The file**: `<git.path>/checkpoints/<name>.json`, the `Checkpoint` JSON of
  `<state>/checkpoints/` pretty-printed with a trailing newline (a reviewer diffs it; the
  local store writes it compact). Its size is checked against 16 MiB when written too.
- **Never by the automatic mode**: `commit` (what the automatic mode and `resolve` call)
  never names a checkpoint; `commit_with` does. Every commit stages
  `add -A -- <path> ':(exclude)<path>/checkpoints'`, unstages that directory first and
  then adds only the files it wrote, so a file left there by hand or by a failed commit
  never reaches a commit. "Nothing to commit" is now `git diff --cached --name-only`
  (was `status --porcelain`, which would count an untracked stray file).
- **Listing**: decided both. A pull answers `checkpoints` (names at the commit it read,
  the remote branch for a dry run) and `GET /_rustybin/checkpoints?from=git` (admin)
  reads them at `HEAD` with a summary, or `ok: false` and the reason. At most 20 listed
  (`truncated`), 64 MiB read in total; a later one is still restored by name.
- **Untrusted**: the size before the read (`413`), then `parse_untrusted` (`422`): the
  name is the file's, `saved_at` short, at most `max_mocks` mocks, valid distinct mock
  names, a revision each within `max_spec_bytes`, stateful items and collections within
  `state_items`; the specs and settings are then compiled and validated by the restore
  exactly as a local checkpoint's.
- **Secrets**: a checkpoint holds no instance secret. The mocks' demo credentials are
  part of their settings; the document a commit writes uses `SecretsMode::Refs`, which
  keeps them as they are, so the checkpoint follows the same rule and shows nothing the
  document does not.
- **MCP**: `list_repository_checkpoints` (admin, read-only) is new, at the end of
  `TOOLS`; its list is under `untrusted_checkpoints` (written by whoever can push).
  `git_commit` gains `checkpoints` (no confirmation: a commit stays local, `git_push` is
  confirmed), `restore_checkpoint` gains `from` (confirmed unless `dry_run`, as before).
- **CLI**: `git commit --checkpoint NAME` (repeatable, or comma separated),
  `checkpoint list --from-git`, `checkpoint restore NAME --from-git`.
- **Tests**: `src/sourcecontrol/tests/checkpoints.rs` (commit, pull on a second
  instance, dry run and restore give the same mocks and data; over the cap `413`,
  malformed, renamed and duplicated `422`; the automatic mode never stages one),
  `a_checkpoint_from_outside_is_held_to_the_local_caps`, the git MCP tool test and
  `git_commit_takes_checkpoints_and_restore_reads_the_repository` (CLI).
- **Docs**: the checkpoint flow is in `docs/examples/config/source-control.sh` (anchors
  `commit_checkpoint`, `pull_checkpoints`, `restore_from_git`), the git examples being a
  shell script against a bare repository rather than Hurl.
- **Not done**: removing a checkpoint from the repository (a git operation outside the
  instance), importing one into `<state>/checkpoints/` (restore from git, then save).
  The tool counts per ceiling in `docs/src/coding-agents.md` were not updated (already
  behind after steps 01 and 02; the release step recounts).
