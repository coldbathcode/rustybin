# Checkpoints

A checkpoint is the state a demo starts from, saved by name and restored before the
next run. Its scope is deliberately narrow:

| Saved and restored | Never touched |
|---|---|
| The instance's mocks created through the API or the console: the deployed revision, a draft that differs from it, on or off, the stateful data | Visitors' self-service mocks, API keys' mocks and mocks with an expiry (a public instance's API mocks) |
| The `/health` toggle | Mocks from `rustybin.yaml` files and the mocks directory, the built-in sample, a [gateway scenario](gateway-scenarios.md)'s mock |
| (on restore) the `/flaky` counters are reset | Settings, secrets, saved tests, captured traffic |

## Save

```hurl
{{#include ../examples/config/checkpoints.hurl:setup}}
```

```hurl
{{#include ../examples/config/checkpoints.hurl:save}}
```

A name is 1 to 64 letters, digits, `.`, `_` or `-`; `replace: true` overwrites a
checkpoint of that name. At most 20 checkpoints, 16 MiB each and 64 MiB together,
kept in `<state>/checkpoints/` (in memory without a state directory).
Saving, restoring and deleting need admin rights; the list and a checkpoint's
contents are readable with the console scope.

## Restore

The demo changes things:

```hurl
{{#include ../examples/config/checkpoints.hurl:change}}
```

`?dry_run=true` says what a restore would change, and changes nothing:

```hurl
{{#include ../examples/config/checkpoints.hurl:dry_run}}
```

A restore brings each saved mock back (`created` when it was deleted, `replaced`
when its revisions or on/off changed, `data_restored` when only its data did,
`unchanged` otherwise), deletes the instance's API mocks the checkpoint does not
hold, sets the health toggle and resets the flaky counters. One restore runs at a
time, and no mock edit lands in the middle of one (a save waits for it too). Each mock
is built first and swapped in with one write, so a mock that cannot be restored keeps
what it had (a problem in the answer), never lost:

```hurl
{{#include ../examples/config/checkpoints.hurl:restore}}
```

Restoring again answers `changed: false`: the instance already is the checkpoint. A
replaced mock starts a new revision history and an empty traffic log; a mock whose
name now belongs to a visitor or a file is left as it is, with a line in `problems`.

```hurl
{{#include ../examples/config/checkpoints.hurl:cleanup}}
```

## In the configuration repository

A team that keeps its instance in git ([source control](source-control.md)) can keep
its demo checkpoints there too, and restore one on another instance after a pull.

**Commit.** `POST /_rustybin/git/commit` takes `checkpoints`, a list of this
instance's checkpoints (default none), and writes each to
`<git.path>/checkpoints/<name>.json` next to `rustybin.yaml`, in the same format as
`<state>/checkpoints/` (pretty-printed, so a reviewer can read the diff). A checkpoint
holds mock data, which may be test data a team does not want in a repository, so it
is written **only by a commit that names it**: the [automatic
mode](source-control.md#automatic-mode) never writes one, and no commit stages
anything else in that directory (a file left there by hand stays out). A checkpoint
holds no instance secret; the mocks' demo credentials are part of their settings and
appear as they do in the `rustybin.yaml` the same commit writes.

```bash
{{#include ../examples/config/source-control.sh:commit_checkpoint}}
```

**List.** A pull answers the repository's checkpoints by name (`checkpoints`); it
never restores one. `GET /_rustybin/checkpoints?from=git` (admin) reads them from the
working copy's last commit (`HEAD`, so pull first), each with what it holds, or
`ok: false` and the reason it cannot be restored. At most 20 are listed (`truncated`
says there are more; a later one is still restored by name).

```bash
{{#include ../examples/config/source-control.sh:pull_checkpoints}}
```

**Restore.** `POST /_rustybin/checkpoints/{name}/restore?from=git` restores the
repository's checkpoint of that name exactly as a local one (`dry_run=true` first),
and answers `from: "git"` with the commit it read. Nothing is saved locally.

```bash
{{#include ../examples/config/source-control.sh:restore_from_git}}
```

A checkpoint from the repository is untrusted input, written by whoever can push:

- its size is checked before it is read, and a file over 16 MiB is refused (`413`),
  never read or cut;
- it must parse as a checkpoint named after its file, with at most the instance's
  number of mocks, valid and distinct mock names, a revision each within the spec
  limit and stateful data within a mock's budget, else `422` with what is wrong;
- its specs and settings are compiled and checked on the compile pool, like a local
  checkpoint's: a mock that does not compile is a problem in the answer and keeps what
  it had.

## Checkpoints and `rustybin.yaml`

A [configuration document](configuration-as-code.md) is the instance's definition, in
git; a checkpoint is a snapshot of the demo's moving parts (stateful data, a draft,
the health toggle) for the next few hours. Apply a document to set an instance up,
and restore a checkpoint between runs.

## On every surface

| Surface | Where |
|---|---|
| API | `GET`, `POST /_rustybin/checkpoints`, `GET`, `DELETE /_rustybin/checkpoints/{name}`, `POST /_rustybin/checkpoints/{name}/restore` (`?dry_run=true`); the repository's: `GET /_rustybin/checkpoints?from=git`, `POST /_rustybin/checkpoints/{name}/restore?from=git`, `POST /_rustybin/git/commit` with `checkpoints` |
| Console | **Management, Checkpoints**: save, the list, **Preview**, **Restore** (confirmed), delete; **In the repository**: the repository's checkpoints with **Preview** and **Restore**. **Management, Source control**: the commit card's checkpoint choice |
| CLI | `rustybin checkpoint list`, `show`, `save`, `restore`, `delete` (`restore --dry-run`); `checkpoint list --from-git`, `checkpoint restore NAME --from-git`, `git commit -m "..." --checkpoint NAME` ([reference](cli.md#checkpoint)) |
| Management MCP | `list_checkpoints`, `save_checkpoint`, `restore_checkpoint` (confirmed unless `dry_run`; `from: "git"`), `delete_checkpoint` (confirmed), `list_repository_checkpoints`, `git_commit` with `checkpoints` ([agent page](agent-mcp.md)) |
| `rustybin.yaml` | Nothing: a checkpoint is a snapshot of the instance, and the document already is one (a committed checkpoint is a file next to it, never a section of it) |
