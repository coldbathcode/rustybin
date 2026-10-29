# Step 12: Docs sweep, conformance, release 0.2.0

Master plan: [master-plan.md](master-plan.md). Needs: all. Status: **not started**. About 1.5 days.

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

(filled in when built)
