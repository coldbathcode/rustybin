# Step 25: Release 0.8.0

Needs the owner's word: it tags and publishes.

## Goal

Whichever of steps 15 to 24 are built reach people in one release.

## What it does

The recipe of step 14, with what these steps add:

- The changelog entry lists each built step under **New**, **Changed** or **Fixed**, from the "For
  the merge" text of its step file; a step that was decided against is named in the roadmap, not in
  the changelog.
- Change-of-reach notes come first in the release notes: step 17 lets a user see and decline their
  own agents' secret requests without `secrets.write`.
- If step 19 is built: the upgrade note says verification is off until turned on, and that turning
  it on trusts the head of that moment.
- If step 22 chose a second image: both digests are recorded and both verified with `cosign`; the
  two image builds run in parallel jobs.
- `docs/src/roadmap.md`: "Under consideration" keeps only what was not built, with the reason a
  decision gave.

## Before the tag

The full checks on `main`: fmt, clippy on both toolchains and with each feature, the test suite with
and without `harness-http3`, the 1.88 builds, `cargo audit`, the docs examples, the include and link
checks, the browser check, the SDK conformance, and `conformance/harness/run.sh` if a step touched
the cluster's messages.

## Done when

`v0.8.0` is tagged, its image or images are signed with their digests in the changelog, the release
page carries the notes and the binaries, and the rows of the steps it holds say done.
