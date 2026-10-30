# Step 12: Docs sweep, conformance, release 0.2.0

Master plan: [master-plan.md](master-plan.md). Needs: all. Status: **done 2026-10-30**. About 1.5 days.

## Goal

The book reads as one product again, every feature of 0.2 has a tested example and a conformance
check, and `v0.2.0` is tagged, published, signed and ready for the platform.

## Read first

- `docs/src/SUMMARY.md`, `docs/src/introduction.md`, `docs/src/quick-start.md`, `README.md`,
  `docs/src/changelog.md`, `docs/src/roadmap.md`, `CLAUDE.md`.
- `conformance/ui/console_e2e.py` and `README.md`, `conformance/mcp/`, `conformance/harness/`.
- `.github/workflows/` (CI, the publish workflow with cosign), the release steps used for 0.1.0
  (the changelog heading, the tag, the GHCR digest, the platform's `pod-image-verify`).

## Design

- **Book structure**: "Getting started" gains the first sign-in; "Using Rustybin" is reordered
  around the jobs: Web console, Bring your own AI, Build mocks with AI, Dev portal, Configuration as
  code and Source control, CLI, Configure from an AI agent, MCP inspector and registry, Load testing,
  Gateway recipes, Exports. Every page lists its environment variables and links the console tab,
  the CLI commands and the MCP tools for the same job (a small "On every surface" table at the end
  of each page).
- **Introduction and README**: the 0.2 story (describe, explore, manage, lock) in the first screen;
  the feature list regrouped; the quick start shows the sign-in, the AI key and the portal in five
  commands.
- **Examples**: every new route has a Hurl example run by `docs/examples/run.sh` (new instances in
  the runner: password mode, the mock assist provider, a portal with published items, a bare git
  repository); `check_includes.py` at 0 problems.
- **Conformance**: `console_e2e.py` covers sign-in, Management tabs, the assist bar with the mock
  provider, Describe it, the inspector against a mock's server and the relay, the registry view,
  the portal in a credential-less context, agents, export and source control; `conformance/mcp/`
  covers the new tools and prompts; `conformance/harness/` unchanged.
- **CLAUDE.md**: the 0.2 modules in the layout, the five-surface rule, the parity tests, the
  settings and egress conventions, the plan folder marked complete.
- **Changelog**: the "Unreleased" section becomes "0.2.0 (date): the AI-assisted mocking service"
  with the change-of-default note for the console lock first.
- **Roadmap**: "Next" empty, "Under consideration" refreshed from the build notes of every step.
- **Release**: `cargo` version 0.2.0, the tag `v0.2.0`, the publish run green with the signature
  verified, the digest recorded in the changelog, the platform's `pod-image-verify` instructions in
  the release notes, the GitHub release created from the tag.

## Done when

All checks green on `main` and on the tag's runs, the book builds with every include resolving, the
release exists with its digest, the plan folder's status table shows every step done; committed
and pushed.

## Build notes

Built 2026-10-30 in one session. Where the build differs from the design above, and why:

- **CI was red since step 02.** The SDK conformance job failed on every push from the sign-in
  step on: `conformance/mcp/run.sh` started its first instance without `RUSTYBIN_CONTROL_AUTH`,
  so the anonymous checks of `test_manage.py` (read tools only, no token) were refused by the
  new default. The runner sets `RUSTYBIN_CONTROL_AUTH=open` for that instance now (the OAuth
  instance keeps `token`). The step 11 walk of `build_instance` had never run green either: a
  spec from the mock provider carries an example on every operation, so `generate_examples`
  rightly answered "nothing to do"; the test asserts that, then generates on a mock without
  examples (`BARE`, the orders spec minus its example) and checks the coverage before and after
  (`coverage.without` is a list of operations in the report). The ai, a2a and oauth suites
  needed nothing.
- **Book structure.** The pages kept their paths (links from docs.rustybin.dev and inside the
  book stay valid); only `SUMMARY.md` moved them: "Using Rustybin" now lists the jobs in the
  plan's order (console, your own AI, AI-built mocks, portal, the instance as code, source
  control, the CLI and its reference, agents, the MCP inspector, the MCP registry, load testing,
  recipes, exports) and comes before the reference parts; `mocks/ai.md`, `mcp/inspector.md` and
  `mcp/registry.md` left their old nesting. "Getting started" did not gain a page: the quick
  start already had "First sign-in" and gained "Five commands" (sign in, the key, a described
  mock, the portal, the export, with the console and the agent equivalents).
- **"On every surface" tables** were appended to the thirteen Using Rustybin pages (API,
  console, CLI, management MCP, `rustybin.yaml`, environment, each row naming the routes,
  verbs, tools, sections and variables with links). The environment variables were already
  listed on every page or in `configuration.md`; the tables link the section.
- **Examples.** No new runner instances: the plan's four (password mode, the mock provider, a
  portal with items, a bare repository) already existed as the eighth instance, the main
  instance's `RUSTYBIN_ASSIST_PROVIDER=mock`, `portal.hurl` publishing its own items and
  `config/source-control.sh`. Twenty-eight catalogue routes under `/_rustybin`, `/portal` and
  `/mcp-registry` had no example; the fourteen from 0.2 got one (ending one sign-in session by
  id, ending an agent's sessions, the agents SSE feed in `live_feeds.sh`, the deploy key, a
  third git instance connected through `git.remote` and `POST /git/init`, a diverged branch
  with the `409` and `resolve`, the registry's health, ping, version, public validate,
  server-level status and `/v0` alias, the portal's `/portal/reference/{id}`). The fourteen
  left are 0.1 routes (harness node-to-node calls, mock duplicate and rule tests, the mock
  state and collections routes, the visit token). Hurl's deprecated `includes` predicate became
  `contains` in every file.
- **Conformance.** `console_e2e.py` already covered every 0.2 view (built step by step); its
  README table did not, and does now. `conformance/mcp/` already walked the new tools and
  prompts (step 11). `conformance/harness/` unchanged.
- **Release.** `cargo` 0.2.0; the changelog's "Unreleased" became "0.2.0 (2026-10-30): the
  AI-assisted mocking service" with the change-of-default paragraph first and the step 10 entry
  moved before step 11; the roadmap's "Next" is empty and "Under consideration" collects the
  leftovers of every step's build notes. The plan's `pod-image-verify` is a platform step that
  is not in this repository: the release notes carry the digest-pinned reference and the two
  cosign values (issuer, identity regexp) a platform verifies with. The digest is recorded in
  the changelog after the publish run, in a commit after the tag (the tag's run is what
  produces it).
- **Checks run**: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (1090
  lib tests plus the integration tests), `python3 docs/tools/check_includes.py` (559 includes,
  0 problems), `mdbook build docs`, `scripts/check-no-dashes.sh`, `docs/examples/run.sh` (60
  files, 0 failed), `conformance/ai|mcp|a2a|oauth/run.sh` (all green, in a venv from
  `conformance/requirements.txt`), `conformance/ui/run.sh` (all good).
