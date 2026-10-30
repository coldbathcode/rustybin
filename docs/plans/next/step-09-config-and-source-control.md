# Step 09: The whole instance as YAML, and git

Master plan: [master-plan.md](master-plan.md). Needs: 01, 07, 08. Status: **done 2026-10-30**. About 3 days.

## Goal

Export everything an instance is into one `rustybin.yaml` (plus the spec files it references) that
can be committed, diffed and reviewed; apply such a file to an instance with a dry run first; and
from Management, Source control, connect the instance to a git repository: commit and push its
configuration, pull and apply the repository's version, and load from the repository at startup so
the repo is the source of truth and the backup.

## Read first

- `src/mocks/config_file.rs` (`MocksFile`, `MockDoc`, loading, env references, export),
  `src/mocks/service.rs` (`apply`, `apply_dry_run`, `export`), `src/mocks/cli.rs` (`check-config`),
  `docs/src/mocks/config.md`, the `rustybin://config.schema.json` resource.
- `src/harness/saved.rs` (saved scenarios), step 01's settings schema, step 06's registry entries,
  step 07's portal items and branding, step 08's agent rules, `src/keys/registry.rs` (the keys file
  stays a separate file referenced by path: secrets never go into the config).
- `Dockerfile` (the runtime image gets `git`), `src/server.rs` (startup loading and SIGHUP reload).
- `src/egress.rs` (`Purpose::Git`).

## Design

### `rustybin.yaml` version 2

```yaml
version: 2
instance:
  title: Payments team mocks
  console: { user: rustybin }          # the password is never here
settings:                              # non-secret settings by dotted key
  assist.provider: openai
  assist.model: ...
secrets:                               # references only; values come from the environment at load
  assist.openai_key: ${RUSTYBIN_ASSIST_OPENAI_KEY}
mocks: [...]                           # as today (version 1 files still load)
registry: { scenario: normal, servers: [...] }
portal: { title, intro, accent, auth, items: [...] }
harness: { scenarios: [...] }          # the saved scenarios inline or by file
agents: { default_ceiling, rules: [...] }
health: { initial: healthy }
```

- `src/configdoc.rs`: the document model (every section typed, `deny_unknown_fields`), `load`
  (version 1 is upgraded on read), `export(instance, Redaction)`, `diff(a, b)` (a section-aware
  diff that names what changes: "mocks.petstore: spec changed (3 operations added), portal.items:
  +mcp:main"), `apply(doc, prune, dry_run)` that delegates to each section's service (mocks as
  today, settings store, registry, portal, saved scenarios, agent rules). Specs can be inline
  (`spec_inline`) or files next to the document (`spec: specs/petstore.yaml`); a bundle export
  writes the directory.
- Routes: `GET /_rustybin/config/export?format=yaml|bundle&secrets=refs|omit` (admin; a bundle is a
  tar of the directory), `POST /_rustybin/config/apply` (`{document, prune, dry_run}`, admin,
  answers the diff and the result), `GET /_rustybin/config/schema` (the JSON Schema of version 2;
  the MCP resource and the CLI's completion use it). `RUSTYBIN_CONFIG_FILE` loads a version 2
  document at startup (every section) and on SIGHUP.
- Secrets: never exported as values. `secrets: {key: ${ENV}}` references resolve at load from the
  environment; a stored secret with no environment reference exports as `secret:stored` so a
  reviewer sees that the instance holds one.

### Source control (`src/sourcecontrol.rs`)

- Needs the `git` binary (added to the runtime image; found on `PATH` elsewhere; the feature says
  "git is not installed" otherwise). All operations run `git` as a subprocess with a timeout in a
  working copy at `<state>/repo` (so `RUSTYBIN_STATE_DIR` is required), with `GIT_TERMINAL_PROMPT=0`,
  a credential helper that reads the token from the settings store for the one remote host, or a
  deploy key the instance generates (ed25519 in `<state>/repo.key`, 0600; the public key shown in
  the console to add to the repository). The remote host must pass the egress policy
  (`Purpose::Git`: added as a default when configured).
- Settings: `git.remote`, `git.branch` (default `main`), `git.path` (a directory in the repository,
  default `rustybin/`), `git.auth` (`token` | `deploy_key`), secret `git.token`, `git.author`
  (`Rustybin <rustybin@example.invalid>` default), `git.auto` (`off` | `push_on_change` |
  `pull_every: 5m`).
- Operations (all admin, all audited, all serialised by one lock):
  - `GET /_rustybin/git` status: remote, branch, last commit, ahead and behind, whether the
    working copy differs from the instance (the export is regenerated and diffed), last operation
    and its result.
  - `POST /_rustybin/git/init` clone (or init when the remote is empty), `POST /_rustybin/git/commit`
    (`{message}`; exports the bundle into the working copy, commits), `POST /_rustybin/git/push`,
    `POST /_rustybin/git/pull` (`{apply: true, dry_run}`: fetch, fast-forward, then
    `config/apply` with the diff shown first), `POST /_rustybin/git/key` (generate or show the deploy
    key). No merges: the instance never resolves conflicts; a non fast-forward pull reports it and
    offers "replace the repository's copy with this instance's" (a new commit) or "replace this
    instance with the repository's" (apply with prune).
  - History: the last 50 commits with their diff summaries (from `git log`), shown in the console.
- Startup: `RUSTYBIN_CONFIG_GIT=<url>` with `RUSTYBIN_CONFIG_GIT_REF`, `RUSTYBIN_CONFIG_GIT_PATH`,
  `RUSTYBIN_CONFIG_GIT_TOKEN` or the deploy key file: clone into the state dir and apply before
  serving (a failure is fatal only with `RUSTYBIN_CONFIG_GIT_REQUIRED=true`; else warn and start
  empty). `git.auto: pull_every` keeps it in sync; `push_on_change` commits after every console,
  CLI or MCP change (debounced 30 s) with an automatic message naming the change.

### The console

- Management, **Export**: the document as YAML with copy and download, the bundle download, the
  diff against the last export, "Apply a document" (paste or upload, dry run with the diff, then
  apply, prune as an explicit checkbox).
- Management, **Source control**: the remote form (URL, branch, path, auth with the token field or
  the generated public key to copy), Status (ahead, behind, drift between instance and working
  copy), Commit and push (message, a preview of the diff), Pull (dry-run diff, then apply), the
  history, the auto mode, and a timeline of operations with their outcomes. Sparkle: "Write the
  commit message" from the diff summary.

### CLI and MCP notes

Step 10: `rustybin config export|apply|diff|schema` (local file or remote instance), `rustybin git
status|init|commit|push|pull|key`. Step 11: `export_config` and `apply_config` grow to version 2,
`git_status`, `git_commit`, `git_push` (confirmed), `git_pull` (confirmed when applying),
`diff_config`.

## Tests

- Unit: the document round trip for every section, version 1 upgrade, the diff, secret
  references, bundle export and load.
- Full app: export then apply on a second instance gives the same export; dry run changes nothing;
  prune; startup from `RUSTYBIN_CONFIG_FILE` version 2.
- Source control against a bare repository in a temp directory (`git init --bare`, `file://`
  remote with the loopback and file schemes allowed in tests): init, commit, push, pull and apply,
  a non fast-forward reported, the deploy key generated, `RUSTYBIN_CONFIG_GIT` at startup. Tests
  skip with a notice when `git` is missing (CI has it).
- Console check: export, apply a document with a dry run, the source control form.

## Docs

`docs/src/configuration-as-code.md` (new: the version 2 file, every section, secrets, apply and
dry run, the bundle) replacing the mocks-only page's scope (`mocks/config.md` points to it),
`docs/src/source-control.md` (new: connect a repository, deploy keys and tokens, push and pull,
startup from git, conflicts), `configuration.md`, `console.md`, the Dockerfile comment, the
changelog, Hurl and shell examples (a bare repo in the examples runner).

## Done when

An instance exports to a reviewable YAML and bundle, applies with a dry run, and round-trips
through a git repository from the console and at startup; all checks pass; committed and pushed.

## Build notes

Built 2026-10-30. What differs from the plan and why:

- **One model, not two.** There is no separate `configdoc` document type: `MocksFile`
  (`src/mocks/config_file.rs`) is the version 2 document, grown by the sections
  `instance`, `settings`, `secrets`, `harness` and `health`, with `mocks` now an
  `Option` (an absent section is left alone, a present one is the truth when pruning).
  `parse` and `parse_lenient` accept versions 1 and 2, so a version 1 file "upgrades"
  by being a version 2 document with the mocks sections only; `rustybin mock push`,
  `check-config` and `POST /_rustybin/mocks/apply` take both (the mocks route applies
  the `mocks` section only). `src/configdoc/` holds the operations: `export`, `diff`,
  `apply`, `apply_extra` (what a file load applies beyond the mocks, registry, portal
  and agents sections, which `mocks::reload_files` already owned), the bundle
  (`write_dir`, a hand-rolled ustar plus gzip in `tar_gz`, no new dependency) and the
  routes in `api.rs`. The JSON Schema stays `config_file::json_schema()` (version 2;
  the MCP resource and `GET /_rustybin/config/schema` serve it).
- **`portal` carries `items` only.** The portal's title, intro, accent and auth are
  settings and live under `settings` like every other setting; duplicating them in a
  `portal` section would have given two places for one value.
- **Secrets.** `secrets` values are `Option<String>`: `${VAR}` resolves from the
  instance's environment on apply (files are expanded before parsing, as before),
  `secret:stored` keeps the instance's own, `null` unsets, and an empty resolution is
  kept and reported rather than clearing a stored secret by accident. Export writes
  `${VAR}` for an environment-seeded secret (the variable's name) and `secret:stored`
  for a stored one. The mocks' demo credentials are part of the mocks and exported
  unless `secrets=omit`. The `git` settings group is never exported nor pruned: a
  pulled document must not repoint or drop the instance's own repository.
- **Which store each section goes to, in API mode.** Mocks: `Origin::Api` (persisted
  in the mocks state directory, pruning API mocks), the same for a pull and for
  `RUSTYBIN_CONFIG_GIT` at startup, so the repository is the truth and a console mock
  not committed goes at startup. Portal items: `Portal::publish` (persisted in
  `portal.json`; items a `RUSTYBIN_CONFIG_FILE` defines are kept as the file's). Registry
  entries and agent rules: the "file copy" on the `MockStore` (the only slot the
  registry and the agents policy read), persisted in `<state>/config.json` and loaded
  at startup before the config file; published registry entries the document names
  move into that copy and the rest go when pruning. For that the registry's state
  moved from a module-local `Extension` into `AppState.registry`. Saved scenarios:
  `Harness.saved` (a problem when harness mode is off). Health: the toggle.
- **Diff.** `configdoc::diff` is a section-aware list of `Change {section, key,
  action, detail}` (lines such as `mocks.orders: spec changed (2 operation(s) added),
  disabled, responses changed`), capped at 500, computed against the current export
  before anything is applied and answered by every apply (a dry run is the diff plus
  each section's would-do). Specs are compared as parsed documents, so an export
  re-applied is "no change".
- **Source control** is `src/sourcecontrol/` (`mod.rs`, `api.rs`), the state in
  `AppState.git` (the startup task and the automatic mode need it outside the router).
  The working copy is made with `git init -b <branch>`, `remote add`, `fetch` and a
  checkout of the remote branch (one path for an empty and a non-empty remote, no
  clone special cases). Every command runs with `GIT_TERMINAL_PROMPT=0`,
  `GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_NOSYSTEM=1`, `HOME=<state>`,
  `credential.helper=` and `core.hooksPath=/dev/null`, a 60 s timeout and the token
  scrubbed from any output. A token goes through `GIT_ASKPASS` (a script in
  `<state>/repo.askpass` reading two environment variables of the child), a deploy key
  through `GIT_SSH_COMMAND` with `IdentitiesOnly`, `BatchMode` and
  `StrictHostKeyChecking=accept-new` into `<state>/repo.known_hosts`. The egress check
  builds the `Purpose::Git` rule from the remote's host and port and resolves it with
  `egress::pin_with` (ssh remotes have no URL the policy's `resolve` would take); a
  local remote needs the loopback allowance. Added settings beyond the plan:
  `git.username` (the user name sent with a token) and `git.pull_every` (the plan's
  `pull_every: 5m` as its own setting next to `git.auto: pull`). A pull's dry run reads
  the document and its spec files from `origin/<branch>` with `git show` into a
  temporary directory, so the working copy never moves on a dry run. `push_on_change`
  is a 30 s poll comparing the export with the working copy (no change bus exists;
  the first commit's message lists the drift, truncated to 200 characters). The
  conflict resolution is `POST /_rustybin/git/resolve {with: instance | repository}`
  rather than options on pull.
- **Left out.** The mocks-only `GET /_rustybin/mocks/export` still writes version 1
  (unchanged for the CLI and the MCP tool until step 10 and 11 grow them). The
  harness section is inline only (no `file:` references). No merge, no branches per
  environment, no webhook-triggered pull (documented under "Not in this version").
- **Checks.** 1069 lib tests (25 new: the document round trip of every section, the
  version 1 upgrade, the diff, secret references, the bundle and its tar, the routes
  with dry run and prune, startup from a version 2 file, the applied sections across
  a restart, remotes and the policy, init, commit, push, pull and apply across two
  instances, a diverged branch resolved both ways, the deploy key, the automatic
  modes, startup from `RUSTYBIN_CONFIG_GIT`, the routes; the git tests skip with a
  notice without `git`), the docs examples (`docs/examples/config/config.hurl` on the
  secured instance, `source-control.sh` with a bare repository and two instances), the
  browser check (Export with a dry run and an apply, the Source control form).
- **For step 10:** `rustybin config export | apply | diff | schema` over
  `/_rustybin/config/*` (`diff` is an apply with `dry_run`), `rustybin git status |
  init | commit | push | pull | resolve | key | history` over `/_rustybin/git/*`. **For
  step 11:** `export_config` and `apply_config` grow to version 2 (`dry_run`, `prune`),
  `git_status`, `git_commit`, `git_push` (confirmed), `git_pull` (confirmed when
  applying), `diff_config`.
