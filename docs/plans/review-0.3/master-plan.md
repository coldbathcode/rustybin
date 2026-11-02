# Master plan: Rustybin 0.3.1, the second hardening release

Planned 2026-11-02 from a full code review of `main` at `31adc0a` (0.3.0). Eight reviewers read the
code by area (control plane auth, mocks and the data plane, the 0.3 testing workflows, the MCP servers
and agents, the load harness, config with source control, the portal and the CLI, the web console,
and the AI, A2A and older demo modules). Every critical and high finding below was checked against
the source a second time. Two items found during the review are already fixed on `main`
(`686fc3e`): the AI examples apply wrote the draft without a revision check, and the limit-probe test
was flaky under load.

## 1. What 0.3.1 is

No new features. The review found one anonymous request that exhausts memory (the mock LLM's schema
generator), schema walks in the mock engine without a node budget (critical where self-service is
on), credentials that reach callers or places they should not (the console token in the MCP
inspector, a saved connection's query key, filled test variables in captures), a CORS default that
lets any web page sign in with the default password, prunes that delete visitors' and running
scenarios' mocks, and a long tail of bounds, races and redaction gaps.

**Every finding gets fixed in 0.3.1**, as in 0.2.2: a "verify first" finding is closed without a fix
only when its regression test passes on today's code, and the step's build notes say so.

## 2. Principles

As in the hardening plan: a failing test first; one rule per fix, applied everywhere (several
findings are the 0.2.2 rules missing from one more path: the compile pool, schema budgets,
`untrusted_*`, the pinned resolver); the five-surface rule for anything a user sees; bounded work,
not only bounded state; the project's hard rules.

## 3. Severity scale

- **Critical**: one anonymous request takes the process down.
- **High**: crosses a trust boundary (credentials, admin rights, another tenant) or stalls the
  instance with a handful of anonymous requests.
- **Medium**: needs a precondition (a configured feature, a proxy, an authenticated low-rights caller)
  or weakens a documented guarantee.
- **Low**: defence in depth, correctness, or a debug-only panic.

## 4. Steps, order and status

Order is by risk to a public instance. The marker a session looks for is the first row that is not done.

| Step | Title | Worst | Findings | Status |
|---|---|---|---|---|
| [01](step-01-hostile-input.md) | Hostile input: schema budgets, streams, connections | critical | 14 | |
| [02](step-02-secrets.md) | Credentials and third-party content where they belong | high | 14 | |
| [03](step-03-console.md) | Console: the token, stale edits, popups, sign-out | high | 8 | |
| [04](step-04-control-plane.md) | Control plane: CORS, sign-in, keys, agents | high | 13 | |
| [05](step-05-config-and-prune.md) | Config and git: prune scope, untrusted documents | medium | 11 | |
| [06](step-06-mocks-and-workflows.md) | Mocks, checkpoints and tests: pools, locks, persistence | medium | 13 | |
| [07](step-07-harness.md) | Harness: leadership, the kill switch, node calls | medium | 8 | |
| [08](step-08-public-demos.md) | Public demo modules: per-client caps and leftovers | medium | 8 | |
| [09](step-09-release.md) | Docs and release 0.3.1 | | | |

## 5. How a session works

As in the reliability plan: read this file, the step file and CLAUDE.md; a failing test per finding
first, then the fix, then the docs where behaviour changes; fmt, clippy (also `rustup run stable`),
the full suite under the watchdog, `scripts/check-no-dashes.sh`, the docs examples and, when the
console changed, `conformance/ui`. Build notes, the row set to done with the date, push.

## 6. Decisions for the owner

- **CORS for the control plane** (step 04): the plan stops answering cross-origin requests on
  `/_rustybin/*` unless `RUSTYBIN_CORS_ORIGINS` names origins (the data plane keeps `*`). A
  third-party web app that drives the control API from a browser would need its origin listed.
- **Saved tests' read routes** (step 02): admin only, since tests may hold captured bodies. Today a
  console-scope token can read them.
- **The OAuth signing key** (step 04): derived from a random per-instance key in the state directory
  instead of the admin token. Tokens issued before the upgrade stop working once.
