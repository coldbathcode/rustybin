# Step 01: Safe pruning when a definition cannot load

## Goal

An apply whose document names a mock it cannot load (an unreadable spec, a spec that does not
compile, a path refused by the repository rules) never deletes that mock, from any source: the
REST apply, the management MCP, the CLI, a git pull, `RUSTYBIN_CONFIG_FILE` and the mocks
directory reload. The dry run reports exactly what the apply does.

## Fixes

| Where | Problem | Fix |
|---|---|---|
| `src/configdoc/mod.rs` `apply_mocks` | definitions that fail to load are dropped before `MockStore::apply(.., prune)`, which then deletes the existing mock of that name | the names of failed entries are kept out of pruning: pass them to the store as "named, not loaded" |
| `src/mocks/store.rs` `apply` | prunes every API mock not in the loaded list | take the names to keep as well as the definitions |
| `src/mocks/mod.rs` `reload_files` | the same shape for file mocks (a file that fails to parse) | the same rule: a named mock that fails keeps its last good revision and reports the error |
| dry run | `would_delete` computed from all names, execution from loaded ones | one function computes both |

## Tests

An existing mock and a document naming it with an unreadable spec, with `prune`: the mock and its
saved file survive, the answer reports the error, and the dry run's `would_delete` is empty. The
same through a git pull and a file reload. The persistence check section 8.1 asked for: 250
concurrent enable and disable calls, then a restart from the state directory, and the mock comes
back in its last acknowledged state.

## Build notes (2026-11-02)

- `configdoc::apply_mocks` collects every valid name the document carries (`named`),
  computes the dry run's `would_delete` from it, and applies with
  `MockStore::apply_keeping(.., keep)` where `keep` is the named but not loaded;
  `Svc::apply` (`POST /_rustybin/mocks/apply`) does the same. The file reload already
  kept failed names (`mocks::reload_files`), so it needed nothing.
- A spec that loads but does not compile was already safe (`apply_keeping` lists every
  definition, compiled or not).
- The persistence check: 250 concurrent `set_enabled` calls on a multi-threaded runtime,
  then a fresh state from the same directory holds the last acknowledged state
  (hardening step 07's `persist_now`).
