# Step 11: Source control: a divergence resolved by a person, branches, signed commits, `file:` for the harness

## Goal

Four things the 0.2 step 09 and the 0.6 webhook left under "Not in this version", built under the
choices of section 5 of the master plan:

- A diverged branch is still never merged by the instance, and still answers `409`. The ways out are
  calls: name a side (with a preview), or push the instance's side to a new branch for a pull request
  on the host. The diverged state says what differs.
- A branch per environment: list the remote's branches, switch with a preview, start a new branch
  from the instance. The branch stays the `git.branch` setting.
- Signed commits: SSH signatures with a key of their own, off by default, the public key and its
  fingerprint shown.
- `harness.scenarios[].file`: a scenario kept in a file next to the document, as a mock's `spec:` is.

## Why

Two instances that push to one branch end in a `409` that today only offers "one side wins, the
other side is gone". A team that reviews configuration wants the third way: put my side in a pull
request and let a person merge it on the host. Teams with a staging and a production instance keep
one repository with a branch each, and today change `git.branch` by hand and hope. Hosts with
branch protection refuse unsigned commits. A scenario that `rustybin harness run` reads from a
file has to be pasted into the document.

## Design

### A. A divergence

`POST /_rustybin/git/resolve` keeps its shape and grows:

| `with` | What happens | `dry_run` |
|---|---|---|
| `instance` | As before: the local commits are dropped (`reset --hard origin/<branch>`), the instance's export is committed on top of the remote head (its parent is the remote head) and pushed. Never a forced push: when the remote moved in between, the push is refused and nothing on the remote changed. | The changes the commit would make to the repository's document (`configdoc::diff`), nothing written. |
| `repository` | As before: the working copy takes the remote head and its document is applied with prune through `apply_working_copy` (`ApplyOptions.untrusted`, the path a pull uses). | The apply's own dry run against `origin/<branch>`, read from the commit, validated exactly like the apply; the working copy does not move. |
| `pull_request` | New. The instance's export is committed on the local branch when it differs from the last local commit, then the local branch is pushed to a new remote branch (`branch`, or `<git.branch>-instance-<UTC time>`). Never a forced push: a branch that exists on the remote and does not contain the local history is refused. Nothing is applied, `git.branch` and the local branch stay as they are (still diverged), and the answer names the branch and the base to open the pull request against. | The branch name it would push and the commits it would carry. |

After the pull request is merged on the host with a merge commit, the local commits are ancestors
of the remote head: an ordinary pull fast-forwards and applies. After a squash or a rebase merge
the branch still reads as diverged, and `repository` takes the merged result.

**What differs.** `SourceControl::divergence` reads the repository's document at `origin/<branch>`
(through `extract`, the same size-checked reads a pull uses) and answers, when `ahead > 0` and
`behind > 0`: `ahead`, `behind`, the subjects of the commits on each side (ten at most),
`instance` (what resolving with the instance changes in the repository's document) and
`repository` (what resolving with the repository changes here), both as `configdoc::diff` changes
with their summary lines, and `options`. It is in the status (`divergence`, null when not diverged)
and in the `409` of a diverged pull or push.

**The automatic mode never resolves.** `auto_tick` calls `commit`, `push` and `pull` only. A
diverged branch is a `409` there as anywhere, logged once until it changes, shown in the status,
and (new) a `source_control_diverged` diagnostics check. No code path of the automatic mode or the
webhook reaches `resolve`.

### B. Branches

- `GET /_rustybin/git/branches` (`git.read`): `git ls-remote --heads origin` through the policy,
  200 names at most (`truncated`), each with its commit and whether it is the configured one.
- `POST /_rustybin/git/branch` (`git.write`) `{branch, create, apply, dry_run, prune, message}`:
  - **Switch** (`create` false): the branch must exist on the remote. `dry_run` answers the apply's
    dry run of that branch's document (read from `origin/<branch>`, untrusted, validated like the
    apply) and changes nothing. Without it: the `git.branch` setting is stored (refused with the
    reason under `RUSTYBIN_SETTINGS_LOCK`), the working copy checks the branch out, and the
    document is applied (`apply: false` moves the branch only). A failure after the setting was
    stored puts the setting back.
  - **Start** (`create` true): the branch must not exist on the remote. The local branch is created
    at the current local head, `git.branch` is stored, the instance's export is committed
    (`message`, or a default) and the branch is pushed. `dry_run` answers what the commit would
    record.
- The local branch of the old name keeps its commits; nothing is deleted on either side.
- **Never from a document.** The `git` settings group is not exported and not pruned (as before).
  New: a document from the repository never changes a `git.*` setting or secret (`git.` joins
  `REPOSITORY_REFUSED_PREFIXES`; `apply_secrets` refuses the group for an untrusted document), so a
  pusher cannot move an instance to another branch, turn signing off or clear the webhook secret.
  Before this step a pulled `settings: {git.remote: ...}` would have been applied; the test
  `a_pulled_document_never_changes_git_settings` fails first to show it.

### C. Signed commits

- Settings (group `git`): `git.sign` (boolean, default off) and the secret `git.signing_key`: an
  OpenSSH private key without a passphrase, kept as one line (the base64 between the `BEGIN` and
  `END` lines; a pasted key with its lines is normalised). Never the deploy key: a key that pushes
  and a key that vouches are registered in different places at a host.
- `POST /_rustybin/git/signing-key` `{replace}` generates an ed25519 key with `ssh-keygen` and
  stores it as the secret. `GET` answers `{signing: {enabled, key_set, public, fingerprint,
  problem}}`: the public key and its `SHA256:` fingerprint, read from the key itself (the public
  half is a field of the OpenSSH key format), never the private half. The same object is in the
  status.
- A commit with `git.sign` on runs with `gpg.format=ssh`, `user.signingkey=<state>/repo.signing.key`
  and `commit.gpgsign=true`. The key file is written from the secret when it differs (mode `0600`,
  `settings::write_private`), never logged. With signing on and no key, a key git cannot use, or
  git older than 2.34, a commit answers `409` with the reason; it never falls back to an unsigned
  commit. The commit's answer carries `signed`.
- Diagnostics: `source_control_signing` warns when signing is on and cannot work.
- Verifying pulled commits against an allowed-signers list: decided in the build (see the notes).

### D. `file:` for the harness section

A scenario entry is `{name, scenario}` (inline, as before) or `{name, file}`. The pattern is the
mocks' `spec:`:

| Where the document comes from | A mock's `spec:` | A scenario's `file:` |
|---|---|---|
| `RUSTYBIN_CONFIG_FILE` (the operator's file) | relative to the file, or absolute | the same, resolved in `config_file::load_file` |
| The repository (pull, startup, webhook, resolve, switch) | relative, no `..`, no link, inside the repository (`contained_path`), read from the commit by `extract` | the same, through the same two functions |
| The API, a tool, `rustybin config apply` (`POST /_rustybin/config/apply`) | refused: send `spec_inline` | refused: send `scenario` |

A file is YAML or JSON, at most `MAX_SCENARIO_BYTES`, never expanded for `${VAR}` (a spec file is
not either). An entry whose file cannot be read is a problem and is kept: it is named, so a prune
never deletes its saved scenario. The export stays inline (a commit never rewrites a hand-written
`file:` layout into something else than what the instance holds; see the notes).

## Surfaces

| Surface | What is new |
|---|---|
| API | `GET /_rustybin/git/branches`, `POST /_rustybin/git/branch`, `GET` and `POST /_rustybin/git/signing-key`; `resolve` takes `pull_request`, `branch`, `dry_run`; the status carries `divergence` and `signing`; a diverged `409` carries `divergence` |
| Console | Management, Source control: what differs and the three ways out with a preview, a Branches card (list, preview, switch, start), a Commit signing card |
| CLI | `git branches`, `git switch`, `git branch`, `git signing-key`; `git resolve --with pull-request --branch --dry-run` |
| Management MCP | `git_branches`, `git_switch_branch`, `git_signing_key` (end of `TOOLS`); `git_resolve` grows |
| `rustybin.yaml` and settings | `harness.scenarios[].file`; `git.sign`, the secret `git.signing_key` (settings only, never in a document) |

## Tests that fail first

- `a_divergence_says_what_differs_and_the_automatic_mode_never_resolves`
- `a_side_is_previewed_and_the_instance_side_is_never_a_forced_push`
- `the_instance_side_goes_to_a_new_branch_for_a_pull_request`
- `branches_are_listed_and_switched_with_a_preview`
- `a_new_branch_starts_from_the_instance`
- `a_pulled_document_never_changes_git_settings`
- `commits_are_signed_with_the_signing_key_and_never_the_deploy_key`
- `signing_without_a_key_refuses_the_commit`
- `a_scenario_file_is_read_like_a_spec_file` and `a_scenario_file_never_leaves_the_repository`
- the tool, CLI and route tests of each, and the parity tests once the routes exist

## Build notes (built 2026-11-09)

- **Where it lives**: `src/sourcecontrol/branches.rs` (the divergence, the previews, the
  pull request way out, the remote's branches, the switch and the start of a branch),
  `src/sourcecontrol/signing.rs` (the signing key and its public half, the three settings
  a signed commit runs with, the generation), `mod.rs` (`Resolve`, `explain`, `resolve`,
  `instance_document`, the two settings), `api.rs` (three new routes). No new `AppState`
  field. Tests: `src/sourcecontrol/tests/branches.rs` and `tests/signing.rs`, plus the
  tool, CLI, configdoc and browser checks.
- **The order of work, as it was.** The step file came first. Most tests were written
  after the code they cover, in the same sitting, not before it. Two were run against
  the old behaviour to see them fail, because they are about what was already there:
  - `a_pulled_document_never_changes_git_settings`: a document from the repository with
    `settings: {git.branch: ..., git.remote: ...}` was applied. The export never wrote
    the group, so "a document in a repository cannot point the instance elsewhere" held
    only for documents an instance had written. `git.` is now in
    `REPOSITORY_REFUSED_PREFIXES`, and `apply_secrets` refuses the group's secrets for an
    untrusted document (`git.token`, `git.webhook_secret`, `git.signing_key`). The
    operator's own file and an admin's apply through the API are as before.
  - `a_changed_spec_is_a_change_against_the_working_copy`: the drift compared a bundle's
    `spec:` path with itself (both sides read the working copy's file), so a mock whose
    spec changed was never drift and `git.auto: push_on_change` did not commit a
    spec-only change. `instance_document` compares the text the commit would write.
- **Resolve**: one route, as before, with `pull_request`, `branch` and `dry_run`; the
  struct `Resolve` replaces the positional arguments. `instance` and `repository` behave
  as in 0.2 (the tool's description said "a force push"; it never was one, and now says
  so). A preview is not an operation of the timeline. The `409` of a diverged pull or
  push gains `divergence` through `SourceControl::explain`, which the routes and the
  tools call (the automatic mode and the webhook do not pay for the diff).
- **`pull_request` pushes the local branch, not a fresh commit on the remote head.** The
  pull request then carries the real history of both sides and the host does the merge,
  conflicts included, which is what "in a pull request" means; a single commit on top of
  the remote head would have been `instance` under another name. The local branch stays
  diverged on purpose, so the automatic mode cannot quietly push it afterwards. After a
  merge commit an ordinary pull fast-forwards (tested); after a squash the branch still
  reads as diverged and `repository` takes the result (documented, not tested).
- **Branches**: `GET /_rustybin/git/branches` and `POST /_rustybin/git/branch` (switch and
  start in one route: `create`). The switch validates the document with the apply's dry
  run before the working copy moves, stores `git.branch`, checks out, applies; a failure
  puts the setting and the working copy back. Names from the remote that `check_branch`
  refuses are counted (`unlisted`), not listed. `apply` defaults to true for a switch.
- **Signing**: the secret is kept as one line because the settings store refuses control
  characters in a secret; `Spec.normalize` now runs for secrets too (it ran for values
  only), which is the one change to `src/settings.rs`. The public half and the
  fingerprint are computed in Rust from the key (no subprocess for a status; the test
  compares the fingerprint with `ssh-keygen -l`). A secret's `hint` is its last four
  characters: for a private key those are the comment and the padding, never key
  material. No environment variable seeds the key (`git.sign` and `git.signing_key` are
  settings only, like `git.webhook_secret`). Signing was tested with git 2.55 and the
  system OpenSSH on macOS; the "git older than 2.34" refusal is covered only by the
  version parser's test, since no old git was at hand.
- **Verifying pulled commits against allowed signers: left out.** It is not small once
  it has to be right: a host's merge commit (the pull request way out above) carries the
  host's own signature or none, so a pull after a merged pull request would be refused;
  the instance's own key would have to be allowed implicitly; and a replaced key would
  make the branch's head unverifiable. The docs say the instance signs and does not
  verify.
- **`file:`**: `ScenarioDoc.scenario` is now optional next to `file`. `scenario_value`
  (configdoc) reads a file for the diff and the apply through `contained_path`, which
  takes the field's name for its messages; `extract` copies the files out of the commit;
  `config_file::load_file` reads them for the operator's file. `apply_harness` now
  normalises a scenario before validating it (a hand-written one uses the shorthand; an
  exported one is already folded), which also makes a hand-written inline scenario apply.
  **The export stays inline**: a bundle does not write `scenarios/*.yaml`, so
  `tests/snapshots/export.yaml` is unchanged and a working copy written by 0.6 does not
  change shape. `rustybin mocks apply` is mocks only and does not apply the section, so
  the CLI inlines nothing for it (the design's last table row is gone).
- **Surfaces**: API (catalogue, OpenAPI, `NEW_SINCE_0_4`); console (what differs in the
  status, "A diverged branch" with three ways out and previews, Branches, Commit signing;
  the answer of an action now survives the status reload that follows it, which the
  pull's dry run needed too); MCP `git_branches`, `git_switch_branch`, `git_signing_key`
  at the end of `TOOLS`, `git_resolve` grown, the divergence and the branch names as
  `untrusted_divergence` and `untrusted_branches`; CLI `git branches`, `switch`, `branch`,
  `signing-key`, `resolve --with pull-request --branch --dry-run` (`resolve` asks unless
  `--dry-run`, so its table row is no longer `destructive`); settings `git.sign`,
  `git.signing_key`; the document's `harness.scenarios[].file`. Diagnostics:
  `source_control_diverged`, `source_control_signing`.
- **What differs, on both sides.** `configdoc::diff` looks only at the sections its
  second document carries, and an export leaves `instance:` out when it is empty, so a
  title only one side has showed on one side of the divergence. `sides` compares an
  absent `instance:` as an empty one (`a_divergence_names_what_only_one_side_has`). The
  apply's own dry run is unchanged: its `summary` lists the document's changes and its
  `results` what prune would also unset, which the docs now say.
- **One git operation at a time, and the console waits.** The browser check found that
  the console's own reads (the status, the history) made a click answer "another git
  operation is running". That `409` now carries `busy: true`, the console's calls wait
  and ask again (twelve times, 250 ms apart), and a status that came back `busy` is
  asked again instead of drawn without its last commit. The API and the CLI still
  answer the `409` at once. The same check, repeated, found that every reload of the
  status drew the cards again with new inputs, so a commit message being typed, a branch
  name or a list of branches just asked for could vanish under a click: those controls
  are now made once and kept (this was so for the commit message before this step).
- **Checks** (2026-11-09, on the committed tree, the machine shared with two other
  builds): `cargo fmt --check`; `cargo clippy --all-targets -- -D warnings` with cargo
  1.98 and with rustup stable 1.99; `cargo check --all-targets` with 1.88 (the MSRV);
  `cargo test`: 1505 library tests, 12 integration, 2 logging, none failed (one of four
  full runs had three harness tests fail on timing with the load average at 17, a
  leader election, a saturated smoke run and a create-then-read count; each passed
  alone and the next full run passed; nothing was loosened);
  `scripts/check-no-dashes.sh`; `python3 docs/tools/check_includes.py` (697 examples);
  `mdbook build docs`; `RUSTYBIN_DOCS_PORT=19800 docs/examples/run.sh` (76 Hurl files,
  10 shell examples); `RUSTYBIN_UI_PORT=19700 conformance/ui/run.sh` with a venv holding
  playwright, pyjwt and cryptography (115 steps, the five new ones repeated 15 times
  after the fixes above). `conformance/requirements.txt` was not installed: the browser
  check imports none of it. `conformance/harness/run.sh` (the compose cluster) was not
  run: this step does not touch the harness engine.
- **Not done**: deleting a remote branch; a list of local branches; a pull request
  opened through a host's API (the instance pushes the branch, a person opens the
  request); `rustybin config apply` inlining spec or scenario files; an environment
  variable for the signing key; verification of pulled signatures (above).

## For the merge

These files are shared with steps 12 and 13 and were not edited here.

**`docs/src/changelog.md`** (under the next version, **New**):

```markdown
- Source control: a [diverged branch](source-control.md#a-diverged-branch) says what differs
  (`divergence` in the status and in the `409`) and is resolved by a person: `POST
  /_rustybin/git/resolve` takes `instance`, `repository` or `pull_request` (this instance's
  side to a new branch, for a pull request at the git host), each with `dry_run`. Still never a
  merge, never a forced push, and the automatic mode never resolves.
- [Branches per environment](source-control.md#branches): `GET /_rustybin/git/branches`, and
  `POST /_rustybin/git/branch` to switch with a preview or to start a branch from the instance.
  `rustybin git branches`, `switch`, `branch`; the tools `git_branches`, `git_switch_branch`.
- [Signed commits](source-control.md#signed-commits): `git.sign` (off by default) with a signing
  key of its own, the secret `git.signing_key`; `POST /_rustybin/git/signing-key` generates it and
  answers the public key and its fingerprint. `rustybin git signing-key`, the tool
  `git_signing_key`.
- A saved harness scenario [in a file of its own](configuration-as-code.md#a-scenario-in-a-file):
  `file:` in the `harness` section, read like a mock's `spec:`.
```

**Fixed**:

```markdown
- A document pulled from the configuration repository could change the `git.*` settings (the
  remote, the branch, the automatic mode) when someone wrote them into it by hand. It no longer
  can: a different value is a problem and the setting is kept.
- A mock whose spec changed, and nothing else, was not seen as a change against the working
  copy: `git.auto: push_on_change` did not commit it and the status showed no drift.
- A harness scenario written by hand in a document (the shorthand with `target`) was refused
  when applied; it is folded like every other way of saving one.
```

**`docs/src/roadmap.md`**: remove the **Source control** line of "Under consideration"
(merges and conflict resolution, branches per environment, signed commits, `file:` for
the harness section). If the list keeps a source control line: "**Source control**:
verifying the signatures of pulled commits against allowed signers; a pull request
opened through the git host's API."

**`docs/plans/core-next/master-plan.md`**: row 11 to `done 2026-11-09`.

**`CLAUDE.md`** (with the paragraph of the next release):

```markdown
Step 11 of core-next (source control, 2026-11-09; `docs/plans/core-next/step-11-source-control.md`):
`src/sourcecontrol/branches.rs` (a diverged branch: `divergence`, the previews, `pull_request`; the
remote's branches, switching and starting one) and `signing.rs` (signed commits). Rules it left:
the instance never merges and never force-pushes, and no code path of the automatic mode or the
webhook reaches `resolve` (a divergence is a person's call: `instance`, `repository` or
`pull_request`, each with a `dry_run` that validates like the apply); the branch is the
`git.branch` setting, changed only by `switch_branch` or the settings API, never by a document;
a document from the repository never changes a `git.*` setting or secret
(`REPOSITORY_REFUSED_PREFIXES`, the group check in `apply_secrets`); a comparison of the instance
with a repository's document goes through `sourcecontrol::instance_document` (a bundle's `spec:`
path compares a file with itself); the signing key is the secret `git.signing_key`, never the
deploy key, and a commit with `git.sign` on is signed or refused, never unsigned; what a tool
answers about the repository's side (`untrusted_divergence`, `untrusted_branches`) is third-party
content; a file a document names (`spec:`, a scenario's `file:`) goes through
`configdoc::contained_path` and is copied out of the commit by `extract`.
```

**Counts that all three steps move** (recount on the merge): `OLD_RIGHTS` and
`ceilings_keep_their_tool_counts` in `src/mcp/manage/agents/tests.rs` (three rows added
here), the admin tool count in
`users_and_roles_tools_invite_confirm_and_never_carry_a_password` and in
`docs/src/coding-agents.md` (139 to 142 here; the per-ceiling column was moved by this
step's own tools only, plus one for `read-only` and `traffic`, plus three for `full`,
and was not recounted from scratch), the regenerated `README.md` and `docs/src/cli.md`.
`conformance/ui/run.sh` gained two exported variables before its last line (no shared
line edited) and `console_e2e.py` one function and one line in `main`.
