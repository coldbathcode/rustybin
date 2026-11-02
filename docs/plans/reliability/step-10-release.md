# Step 10: Docs and release

## Goal

The book covers every new workflow (a page each, "On every surface" tables, tested Hurl examples),
the changelog has a `0.3.0` entry, the roadmap moves the milestone to "Recently built", and the
release follows the 0.2.0 recipe (tag, signed image, GitHub release, digest commit) once the owner
says go.

## Build notes (2026-11-02)

- The book: a page each for [gateway scenarios](../../src/gateway-scenarios.md), [saved
  tests](../../src/saved-tests.md) and [checkpoints](../../src/checkpoints.md); diagnostics
  open the troubleshooting page; edit conflicts are a section of the mocks page; each with its
  "On every surface" table and tested Hurl examples (`docs/examples/scenarios/`,
  `docs/examples/tests/`, `docs/examples/config/checkpoints.hurl`, `diagnostics.hurl`, the
  `conflict` anchor of `mocks/mocks.hurl`). The task index and the console page list the new
  views and tabs; the agent page says `tools/list` pages at 100.
- The changelog's `0.3.0` entry (new, then behaviour changes), the roadmap (the milestone under
  Recently built, new candidates under consideration), `0.3.0` in Cargo.toml and the OpenAPI
  example.
- Checks: fmt, clippy (Homebrew and `rustup run stable`), the MSRV build, the full suite under
  the watchdog (`harness::limit_tests::limit_probe_measures_the_limit_within_five_percent` and
  the CLI live test each failed once under the full parallel run and passed alone and on the
  rerun: timing under load, untouched by this plan), every docs example, the UI and MCP
  conformance, `scripts/check-no-dashes.sh`.
- Not done: the tag, the signed image, the GitHub release and the digest commit (the 0.2.0
  recipe), which wait for the owner.
