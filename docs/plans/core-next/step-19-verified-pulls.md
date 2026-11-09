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
