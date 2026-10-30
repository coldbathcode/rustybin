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
