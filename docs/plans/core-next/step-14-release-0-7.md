# Step 14: Release 0.7.0

Needs the owner's word: it pushes `main`, tags and publishes an image.

## Goal

Steps 11 to 13 (built and merged 2026-11-09) reach people: a version, a tag, the signed image, the
binaries, the digest in the changelog.

## What it does

The release recipe of the last plans, unchanged:

1. Push `main` (the three steps, their merges, the docs). Wait for CI green; this is the first run of
   the new `harness-http3` job and of the MSRV job with the feature.
2. The changelog's "Unreleased" heading becomes `0.7.0 (<date>): <theme>`; the roadmap's
   "Recently built" entry links it; `Cargo.toml` and `Cargo.lock` say 0.7.0. One commit, pushed, CI
   green again.
3. An annotated tag `v0.7.0` on that commit, pushed. The "Docker image" run on the tag builds the
   multi-arch image (45 to 80 minutes under QEMU) and signs it.
4. The digest from the run's `signed ghcr.io/...@sha256:...` line, verified locally with `cosign
   verify`.
5. `gh release create v0.7.0 --verify-tag` with the notes (the upgrade notes first, the
   digest-pinned reference, the two cosign values); the binaries workflow attaches Linux and macOS
   builds.
6. A follow-up commit records the digest in the changelog.

## Upgrade notes the release carries

- Nothing to do when upgrading: what is new is off until it is turned on or named in a scenario.
- A harness scenario with `servers.chain` needs every server node on 0.7.0; runs without a chain
  are unchanged on a mixed cluster.
- `profiles.json` may hold an `agent` object per profile; an older `rustybin` ignores it.
- A document pulled from the repository no longer changes a `git.*` setting (a fix; a repository
  that relied on it sets them on the instance instead).
- The published image has no HTTP/3 (step 22 decides whether one does).

## Before the tag

- The three checks that ran only on the step 13 branch run on `main`: `conformance/harness/run.sh`,
  `rustup run stable cargo clippy --all-targets -- -D warnings`, and the 1.88 builds (`cargo build
  --locked`, `cargo check --locked` with `quota-postgres` and with `harness-http3`).
- `cargo audit` as CI runs it.
- The three merged agent worktrees under `.claude/worktrees/` and their branches are removed.

## Not in this step

The demo (`deploy/demo`) stays pinned to the 0.6.0 digest; moving it is step 26.

## Done when

`v0.7.0` is tagged, the image is signed and its digest is in the changelog, the release page carries
the notes and the binaries, and this row says done with the date.
