# Step 07: Config as code: the repository is not trusted

## Goal

Write access to the configuration repository gives control of what the document describes, nothing
more: not the host's files, not the process environment, not secret values in answers, not the
default password.

## Findings

| Sev | Where | Problem | Fix |
|---|---|---|---|
| high | `src/configdoc/mod.rs:491-513` (`write_dir`, from `sourcecontrol` `commit_inner`) | `create_dir_all`, `read_dir`, `is_file`, `remove_file` and `write` follow symlinks; a pushed `rustybin/specs -> ../..` makes the next commit delete every file in `<state>` (`settings.json`, `secrets.key`, `repo.key`) and a symlinked `rustybin.yaml` overwrites any writable file | `symlink_metadata` on the directory, `specs/` and each file, refuse symlinks; `-c core.symlinks=false`; check canonical paths stay under the repo |
| high | `src/sourcecontrol/mod.rs:1805` (`read_document`) | `${NAME}` anywhere in a pulled document is expanded from the process environment: a mock example `${RUSTYBIN_SECRETS_KEY}` is served unauthenticated after the next pull | do not expand pulled documents as text; resolve references only in `secrets`, only for variables a secret `Spec.env` names (`RUSTYBIN_CONFIG_FILE` keeps today's behaviour) |
| medium | `src/configdoc/mod.rs` `diff` (`map_changes(.., "secrets", ..)`), `apply_secrets` | after the expansion the diff reports `secrets.<key>: changed to <value>` to REST, the console and the `git_pull` tool; the env-seeded secret becomes a stored override; `drift()` never empties, so `push_on_change` pushes every 30 s | never print secret values in a diff; compare secrets by reference before expansion |
| medium | `src/configdoc/mod.rs` `apply_secrets` prune, `login::check_credentials` | a pruned `secrets` section without `console.password_hash` unsets it and the console accepts `rustybin` / `rustybin`; `check_startup` only checks at startup | exempt the `signin` group from pruning; refuse to unset the hash at runtime in public mode when that lands on the default password |
| medium-high | `src/configdoc/mod.rs:306-315`, `src/harness/api.rs:170-178`, `src/harness/saved.rs` | saved harness scenarios export OAuth `client_secret` and API key values in plain text to `rustybin.yaml` and git, and the scenario GET returns them | `report::redact_scenario` (or `${VAR}` / `secret:stored`) in the export and GET; seal stored values with `Settings::seal` |
| medium-low | `src/mocks/persist.rs:96-104`, `src/mocks/store.rs:371-375` | saves run after the store lock is released with a shared temp name: torn or stale files, a deleted mock resurrected on restart, umask permissions on files holding demo credentials | save under the entry's lock or with a revision check; unique temp name, fsync, 0600 via `settings::write_private` |
| low | `src/portal/mod.rs:738-752`, `src/settings.rs:498` | `flush_soon` checks and saves outside one lock, `write_private` uses `tmp.<pid>`; public `POST /portal/api/events` can trigger concurrent saves | one mutex across the check and save; a random temp suffix in `write_private` |
| low | `src/cli/commands.rs:287`, `mcp save-connection --secret` | `git key --generate --replace` is not marked destructive; a secret passed in argv | mark destructive; read secrets from stdin or a file |

## Tests

A repository with a symlinked `specs` and a symlinked document: commit refuses and touches nothing
outside the repo; a pulled example holding `${RUSTYBIN_ADMIN_TOKEN}` is served literally; a pull
diff with an env-seeded secret shows no value and no change; a prune without the hash keeps it; an
export with a harness OAuth secret shows a reference.

## Done when

The tests pass and `docs/src/source-control.md` states what a repository writer can and cannot do.

## Build notes (2026-11-02)

- Every finding is fixed; none was closed as not reproducible. Section 6's decision:
  literal secret values in a pulled document are refused.
- `configdoc::write_dir_within(export, dir, root)` refuses a symbolic link at the
  directory, `specs/`, each spec file and the document, and checks both directories
  resolve under the working copy; stale specs are removed by `file_type` (a link is
  removed as a link). git runs with `core.symlinks=false`.
- Beyond the plan: a pulled document's `spec:` path could name any readable file
  (`../../settings.json`, `/etc/hosts`). `contained_path` now requires a relative
  path to a plain file inside the working copy (configdoc's `base` is only ever a
  working copy).
- `read_document` no longer expands the text. `ApplyOptions.untrusted` (the git
  apply) lets a secret read only `${VAR}` naming its own `Spec.env`; anything else is
  a problem. Diffs show references or "a value". The drift loop ends: the same
  reference is no change.
- Pruning skips the `signin` group's secrets; `Settings` refuses to clear
  `console.password_hash` (update or unset) on a public instance without a password
  from the environment.
- Harness: the export uses `report::redact_scenario_with(.., "secret:stored")`, the
  apply fills the markers from the saved scenario of the same name (`fill_stored`,
  refused when none holds them), the scenario GET answers the redacted scenario, and
  `SavedScenarios` seals each file with the instance's secrets key when it survives a
  restart (files written by `write_private`, 0600).
- Mock persistence: `MockStore::persist_now(name)` writes the store's current state
  under a persistence lock (or removes the file when the mock is gone), so the last
  write is the latest state; files go through `write_private` (unique temporary
  name, fsync, 0600). `write_private` itself now uses a random temporary suffix.
- Portal: `flush_soon` checks and claims the flush under one lock, and `save` is
  ordered by `save_lock`.
- CLI: `git key` is destructive (asks, or `--yes`); `mcp save-connection --secret`
  takes `name=-` (stdin or a prompt) or `name=@file` and refuses a value on the
  command line.
