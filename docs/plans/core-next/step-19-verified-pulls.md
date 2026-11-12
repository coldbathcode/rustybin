# Step 19: Pulled commits verified against allowed signers

Owner decision first (section 6): how a git host's own commits are treated.

## Goal

Step 11 signs what the instance commits and verifies nothing it pulls. An instance that applies its
repository on a pull, a webhook or a timer applies whatever reaches the branch. With verification on,
it applies only what a named signer signed.

## Why step 11 left it out

A host's merge commit carries the host's signature or none, so a pull after a merged pull request
would be refused; the instance's own key has to be allowed without being listed; a replaced key must
not make the branch unverifiable.

## Design (the recommended choice)

- Settings, group `git`: `git.verify` (off by default) and `git.allowed_signers` (public keys with
  their names, the format `ssh-keygen -Y verify` reads; not a secret). A document from the
  repository never changes either (`REPOSITORY_REFUSED_PREFIXES` already holds `git.`).
- The instance's own signing key is allowed implicitly while it is the current key.
- What is verified: every commit between the last verified head and the new one, not only the tip.
  The last verified head is kept in the state directory, so replacing a key never reopens history.
  Turning verification on trusts the head of that moment and says so.
- A commit with one parent passes when an allowed signer signed it (SSH signatures only).
- A merge commit passes unsigned only when both parents pass and its tree is exactly what merging
  them gives (`git merge-tree --write-tree`, git 2.38 or later). A merge that carries a change of
  its own is refused, so a host's merge button works and nothing rides in on it.
- Squash and rebase merges made by the host are signed by the host or by nobody, so they are
  refused. The docs say to merge with a merge commit, or to push signed commits.
- A refusal is `409` with `reason: unverified`, the commit and why; nothing is applied and the
  working copy does not move. The automatic mode and the webhook report it and never apply.
  Diagnostics gain a check (verification on with no signers, git too old).
- `resolve` with `repository`, `switch_branch` and a checkpoint read from git go through the same
  verification, since each applies the repository's side.

## The other choices

- Also trust the host's web signature: needs a GnuPG binary and the host's keys in the image, and
  trusts every commit the host makes for anyone who can click merge. More convenient, weaker.
- Verify the tip only: simple, and a signed commit on top of an unsigned one passes. Not offered.

## Surfaces

API (the status carries `verification`; the `409`), console (a card next to commit signing: the
switch, the signers, the last verified head), management MCP (the git tools answer the refusal;
signers under `untrusted_*` when they come from elsewhere), CLI (`rustybin git signers`), the
settings.

## Tests that fail first

- A signed commit by an allowed signer pulls; the same commit unsigned, or signed by another key,
  is refused and nothing changes.
- A clean merge commit of two verified parents passes; a merge carrying its own change is refused.
- A signed commit on top of an unsigned one is refused (every commit, not the tip).
- A replaced signing key: old history stays trusted, new commits need the new key.
- The automatic mode and the webhook meet a refusal and apply nothing.

## Done when

An instance with verification on applies only signed work on every path that applies the
repository's side, and `docs/src/source-control.md#signed-commits` says what it accepts and what a
team does about squash merges.

## Build notes (built 2026-11-12)

The owner chose the recommended design; "the other choices" are not built.

- **Where it lives**: `src/sourcecontrol/verify.rs` (the signers, the verified heads, the
  check, the status' `verification`), one call of `SourceControl::verify_rev` on each path
  in `mod.rs`, `branches.rs` and `checkpoints.rs`, the two settings in `specs()`. No new
  `AppState` field, no new route, no new tool. Tests: `src/sourcecontrol/tests/verify.rs`
  (real `git` and `ssh-keygen` in temporary directories; skipped with a notice without
  them or with a git older than 2.38), plus the tool, CLI and browser checks.
- **The order of work, as it was.** The tests were written with the code, not before it.
  Then each protection was removed once and the tests run: all thirteen removals failed
  at least one test (the `verify_rev` call of the pull, of `init`, of `resolve` with
  `repository`, of its preview, of the switch, of its preview, of the checkpoint list and
  of the checkpoint read; the merge tree comparison; every commit and not only the tip;
  an own commit trusted only on a verified head; off and on again noticed; the status
  and fingerprint of a signature). No test was seen failing before its code existed.
- **The setting is a list, not an allowed signers file.** `git.allowed_signers` holds at
  most 50 lines `name key-type base64-key` (a `.pub` line is accepted and normalised, its
  comment becomes the name). Options (`cert-authority`, `namespaces`, validity dates) and
  wildcard names are refused. The file `ssh-keygen` reads is written by the instance to
  `<state>/repo.allowed_signers`, every line with `namespaces="git"`. Besides `%G?` being
  `G`, the signature's fingerprint (`%GF`) must be one of the allowed keys', and the
  OpenPGP and X.509 programs are set to `false`, so such a signature reads as none.
- **The verified heads** are kept per branch (at most 50) in `<state>/repo.verified.json`,
  next to the head trusted when verification was turned on; all of them bound the range
  (`git log <new> --not <heads>`), so switching between branches re-verifies nothing.
  A head the working copy does not hold is skipped; a file that cannot be read trusts
  nothing. At most 500 commits are verified in one go.
- **Turning it on** is noticed lazily (at the next git operation, status read or tick of
  the automatic mode), and off and on again between two looks is noticed through a new
  counter in the settings store (`Settings::changes_of`, the one change to
  `src/settings.rs`). The timeline gets an operation `verify` saying which head is
  trusted. With no commit yet nothing is trusted: the first checkout of `init` (and so
  startup from `RUSTYBIN_CONFIG_GIT`) is verified whole. There is no variable that seeds
  `git.verify` or `git.allowed_signers` (like `git.sign`), so an instance that must verify
  from its very first clone needs its settings stored before it connects.
- **The instance's own commits.** A commit the instance makes on a verified head is the
  verified head from then on, signed or not (it made it itself). On an unverified head
  it is not: after `resolve` with `instance` on top of commits nobody signed, the head is
  not verified, a pull with apply and a checkpoint from the repository are refused, and
  the way out is a person's: `git.verify` off and on again trusts the head as it is.
  `resolve` with `instance`, `pull_request` and a push take nothing from the repository
  and are not checked (the `pull_request` path is untouched for step 20).
- **Merges.** A merge of two parents an allowed signer did not sign passes on the tree
  comparison alone, whatever other signature it carries (a host's web signature reads as
  none). Signed by an allowed signer it passes with a change of its own. A merge of more
  than two parents needs a signature.
- **git 2.38 for all of it**, not only for merges: with an older git every verification
  is refused with the reason, and the status and the `source_control_verification`
  check say so. Not tested against an old git (none at hand), like step 11's 2.34.
- **The refusal** carries the commit's id and a reason in the instance's own words, never
  the commit's author or subject, so nothing in it is third-party content. The signers in
  the status are the instance's own setting, so they are not under `untrusted_*` (no
  signer comes from elsewhere in this build). A dry run and a preview are refused like
  the operation. The webhook still answers `202` (the pull is queued); the refusal is in
  the timeline, the status and the diagnostics.
- **Checkpoints**: `checkpoints::Refused` gained a third field (the extra fields of the
  answer), so `?from=git` answers `reason`, `commit` and `why` too.
- **Surfaces**: API (`verification` in `GET /_rustybin/git`, the `409`, OpenAPI and
  catalogue descriptions); console (a "Verified pulls" card after "Commit signing": the
  switch, the signers, the last verified head, the head trusted at turn-on, the last
  refusal; a row and a notice in the status); MCP (`git_status` answers `verification`
  and its summary names a refusal, the git tools and the two checkpoint tools answer
  `reason`, `commit`, `why`; changing `git.verify` through `update_settings` is confirmed
  as every `git.*` setting already was); CLI (`rustybin git signers` with `--add` and
  `--remove`, through the status and the settings routes); settings `git.verify`,
  `git.allowed_signers`; diagnostics `source_control_verification`. `rustybin.yaml`: never
  (the `git` group is not exported and a document from the repository cannot change it;
  tested).
- **Not built**: a host's web signature, tip-only verification, signers read from the
  repository, key validity dates, a route that trusts a head by name.

## For the merge

Changelog, under "Unreleased":

**New**

- **Verified pulls** ([source control](source-control.md#verified-pulls)): with the setting
  `git.verify` on (off by default), the instance takes the repository's side only when an
  allowed signer signed every commit since the last verified head (SSH signatures; git
  2.38 or later). `git.allowed_signers` names who may sign (a name and a public key each,
  at most 50); the instance's own signing key is allowed without being listed. A merge
  commit passes unsigned only as the clean merge of two verified parents, so a host's
  merge button works; a squash or rebase merge made by the host is refused. It holds on
  every path: a pull and its dry run, the webhook, the automatic mode, startup from the
  repository, `resolve` with `repository`, a switch of branch, and a checkpoint from the
  repository. A refusal is `409` with `reason: unverified`, the `commit` and `why`; nothing
  is applied and the working copy does not move. The status answers `verification` (the
  signers, the last verified head, the last refusal), the console has a "Verified pulls"
  card, `rustybin git signers` lists and changes the signers, and the diagnostics gain
  `source_control_verification`.

**Changed**

- `GET /_rustybin/git` answers `verification`; the routes and tools that take the
  repository's side may answer `409` with `reason: unverified` when `git.verify` is on.
- A refused read of the repository's checkpoints (`?from=git`) carries the extra fields
  of the refusal.

**Fixed**

- Nothing.

Upgrade note: nothing changes until `git.verify` is turned on. Turning it on trusts the
head of that moment as it is; connect the repository first and turn it on after. Teams
that squash or rebase pull requests at the git host switch to merge commits (or push
signed commits) before turning it on.

Rules for CLAUDE.md (core next step 19, `src/sourcecontrol/verify.rs`): every path that
takes the repository's side (moves the working copy to it, applies its document, reads
its checkpoints) calls `SourceControl::verify_rev` before anything moves, a dry run
included, and a new such path does too; an operation that moves the head without
verifying (`resolve` with `instance`, a commit) calls `verify_sync` first, so a head is
trusted only as it was when `git.verify` was turned on; the instance's own commit is a
verified head only on a verified head (`own_commit`); a refusal names the commit's id
and a reason in the instance's words, never the commit's author or subject; signatures
are SSH only (the OpenPGP and X.509 programs are never run) and a signer line never
carries an option; the verified heads live in `<state>/repo.verified.json`, in no
document, and a document from the repository never changes `git.verify` or
`git.allowed_signers`.

Shared tables touched: none (no new route, tool or permission; `COMMANDS` gained the
row `git signers`, and `docs/src/cli.md` was regenerated).
