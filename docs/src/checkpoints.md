# Checkpoints

A checkpoint is the state a demo starts from, saved by name and restored before the
next run. Its scope is deliberately narrow:

| Saved and restored | Never touched |
|---|---|
| The instance's mocks created through the API or the console: the deployed revision, a draft that differs from it, on or off, the stateful data | Visitors' self-service mocks and API keys' mocks |
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
hold, sets the health toggle and resets the flaky counters:

```hurl
{{#include ../examples/config/checkpoints.hurl:restore}}
```

Restoring again answers `changed: false`: the instance already is the checkpoint. A
replaced mock starts a new revision history and an empty traffic log; a mock whose
name now belongs to a visitor or a file is left as it is, with a line in `problems`.

```hurl
{{#include ../examples/config/checkpoints.hurl:cleanup}}
```

## Checkpoints and `rustybin.yaml`

A [configuration document](configuration-as-code.md) is the instance's definition, in
git; a checkpoint is a snapshot of the demo's moving parts (stateful data, a draft,
the health toggle) for the next few hours. Apply a document to set an instance up,
and restore a checkpoint between runs.

## On every surface

| Surface | Where |
|---|---|
| API | `GET`, `POST /_rustybin/checkpoints`, `GET`, `DELETE /_rustybin/checkpoints/{name}`, `POST /_rustybin/checkpoints/{name}/restore` (`?dry_run=true`) |
| Console | **Management, Checkpoints**: save, the list, **Preview**, **Restore** (confirmed), delete |
| CLI | `rustybin checkpoint list | show | save | restore | delete` (`restore --dry-run`) ([reference](cli.md#checkpoint)) |
| Management MCP | `list_checkpoints`, `save_checkpoint`, `restore_checkpoint` (confirmed unless `dry_run`), `delete_checkpoint` (confirmed) ([agent page](agent-mcp.md)) |
| `rustybin.yaml` | Nothing: a checkpoint is a snapshot of the instance, and the document already is one |
