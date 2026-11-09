# Step 22: An image with HTTP/3

Owner decision first (section 6): whether a published image carries a QUIC stack, and which.

## Goal

HTTP/3 targets exist only for people who build from source with `harness-http3`. Most people run
the image. Someone who wants to load test an HTTP/3 gateway pulls an image that can.

## The choices

- **A second image, tagged `<version>-http3`** (recommended): built with `CARGO_FEATURES="quota-postgres
  harness-http3"`, signed the same way, its digest in the changelog next to the default one. The
  default image, the one a public instance runs, gains nothing.
- **In the default image**: one image to explain, and every instance carries a QUIC stack it will
  mostly never use, including the public demo.
- **No image**: binaries from source only, as today.

## What the second image costs

- The multi-arch build under QEMU takes 45 to 80 minutes; a second image doubles the release's wait
  unless the two build in parallel jobs.
- `cargo audit` and the MSRV job already cover the feature (step 13). The image's own checks (the
  read-only root, the 512 MiB limit of the hosted demo plan) are run once on the variant.
- A release has two digests to record and verify.

## What it does

- `.github/workflows/docker-publish.yml`: a matrix over the feature sets, the variant's tags
  suffixed, both signed, both digests printed in the `signed ...` line the release reads.
- `Dockerfile`: nothing new, `CARGO_FEATURES` is already a build argument.
- The release binaries stay on the default features unless the owner says otherwise.
- Docs: the quick start and `docs/src/harness.md#http3-targets` name the variant and its cosign
  command; `GET /_rustybin/harness` already says whether the build has `http3`.
- The compose cluster example gains a note on running the variant on every node of a cluster that
  sends HTTP/3.

## Tests

The variant boots under the demo's limits and reports `http3: true`; the default image reports
`false` and refuses an `h3` scenario by the feature's name; `cosign verify` passes on both.

## Done when

The chosen option is built into the release workflow and documented, or this row says no image.
