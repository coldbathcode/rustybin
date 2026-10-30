# Master plan: Rustybin 0.2.2, the hardening release

Planned 2026-10-30 from a full code review of `main` at `1328b07` plus the uncommitted assist
model catalogue. Nine reviewers read the code by area (control plane auth, outbound requests,
mocks and the data plane, MCP servers and agents, the load harness, config and source control
with the portal and CLI, the web console, the older demo modules, and the work in progress).
Every critical and high finding below was checked against the source by a second reader.

## 1. What 0.2.2 is

No new features. 0.2.2 closes the gaps a public instance exposes: one anonymous request that can
abort or stall the process, credentials that travel to targets a caller picks, a console that sends
its admin token wherever third-party content points, revocations a restart undoes, and a git
repository that can reach the host's files and environment. It also finishes the assist model
catalogue that is uncommitted on `main`.

**Every finding in the step files gets fixed in 0.2.2.** None is deferred to a later release,
whatever its severity: the lows ship with the criticals. A finding is closed without a fix only
when it is marked "verify first" and its regression test passes on today's code, and the step's
build notes then say so. The release (step 10) does not start while a step has an open finding.

## 2. Principles

- **A failing test first.** Every finding starts as a regression test that fails on today's code.
  A finding marked "verify first" below is fixed only if its test fails; if it passes, the step's
  build notes say so and the finding is closed.
- **One rule per fix, applied everywhere.** Several findings are one mistake in several places
  (a saved connection's credentials sent to the caller's target in three code paths; the console
  inferring the bearer from the URL in three callers). Fix the rule once, then every caller.
- **The five-surface rule still holds.** A new setting (for example the trusted forward header) lands
  on the API, console, management MCP, CLI and `rustybin.yaml` with its parity tests.
- **Bounded work, not only bounded state.** Request paths get byte budgets and CPU budgets, and
  parse or compile work leaves the async runtime (`spawn_blocking` behind a small semaphore).
- **Project hard rules.** No em or en dashes, no panics on request paths, vendor neutral.

## 3. Severity scale

- **Critical**: one anonymous request takes the process down.
- **High**: crosses a trust boundary (credentials, admin rights, another tenant) or stalls the
  instance with a handful of anonymous requests.
- **Medium**: needs a precondition (a configured feature, a proxy, an authenticated low-rights caller)
  or weakens a documented guarantee.
- **Low**: defence in depth, correctness, or a debug-only panic.

## 4. Steps, order and status

Order is by risk to a public instance, except step 01, which is small and unblocks CI on the
working tree. The marker a new session looks for is the first row that is not done.

| Step | Title | Worst | Needs | Days | Status |
|---|---|---|---|---|---|
| [01](step-01-finish-assist-models.md) | Finish the assist model catalogue | high | | 1 | |
| [02](step-02-hostile-input.md) | Hostile specs and queries: memory and CPU budgets | critical | | 2 | |
| [03](step-03-mcp-credentials-and-agents.md) | MCP: saved credentials, self-calls, agent identity, untrusted content | high | | 1.5 | |
| [04](step-04-console-token-scoping.md) | Console: the control token only where the console chose to send it | high | | 1.5 | |
| [05](step-05-sessions-and-sign-in.md) | Sign-in: revocations that survive, rate limits that hold | medium | | 1.5 | |
| [06](step-06-egress-and-git-transport.md) | Egress pinning and the git transport | high | | 1 | |
| [07](step-07-config-and-source-control.md) | Config as code: the repository is not trusted | high | 06 | 1.5 | |
| [08](step-08-public-data-plane.md) | Public instances: bins, sessions, visits, the demo IdP | high | 05 | 1.5 | |
| [09](step-09-harness.md) | Harness: one run at a time, cluster leadership, panics | high | | 1.5 | |
| [10](step-10-dependencies-and-release.md) | Dependencies, docs, release 0.2.2 | medium | all | 1.5 | |

About 14 days of build sessions. Steps 02 to 06 and 09 are independent of each other.

## 5. How a session works

Read this file, the step file and CLAUDE.md. Write the failing tests named in the step, then the
fixes, then the docs. Run `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`,
`scripts/check-no-dashes.sh` and, when docs changed, `docs/examples/run.sh`. Add the step's build
notes (where the build differs from the plan, findings closed as not reproducible), set the row
to done with the date, and push only when the user says go.

## 6. Findings that need a decision from the owner

- **Join token means cluster control** (step 09): a holder of the join token can join as an
  eligible node today. Either document it as intended or add an operator approval for eligible
  nodes.
- **The demo IdP on public instances** (step 08): limiting the demo clients' redirect URIs on a
  public instance changes how gateway demos that redirect to arbitrary hosts behave.
- **Provider aliases** (step 01): accept `claude`, `google` and `openai-compatible` again in
  `assist.provider`, or document the break in the changelog.
- **Secrets from git** (step 07): refuse literal secret values in a pulled document, or accept them.

## 7. Risks

- Making the console's bearer opt-in (step 04) touches every control call in `ui/`. The UI
  conformance run (`conformance/ui/run.sh`) is the safety net and must run in that step.
- Persisting the token generation (step 05) changes restart behaviour; a deployment that relied on
  a restart to sign everyone out still gets that through `DELETE /_rustybin/sessions`.
- Replacing `serde_yaml` (step 10) can change YAML output byte for byte, which shows up as a diff in
  every git-backed instance. Pin the output format with snapshot tests before swapping.
