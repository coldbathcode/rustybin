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

## 8. Queued follow-up: instance reliability and testing workflows

Added 2026-10-30 from a further code and feature review. **Status: queued, not started.**
Resolve the existing ten steps in section 4, including the 0.2.2 release, before starting this
appendix. Their order, scope and release gates stay as written. These additions are a subsequent
milestone, with its release version and estimates to be decided after the hardening work.

The review included the uncommitted assist model catalogue. The observations and test results
below describe that working tree; recheck them after the earlier steps land. Work already owned
by a hardening step stays in that step and must not be postponed to this appendix or implemented
twice.

### 8.1 Review findings and ownership

The review used P1 for urgent correctness fixes and P2 for the next priority; these labels do
not replace the severity scale or ordering of the existing hardening plan.

| Finding | Evidence and impact | Owner and completion check |
|---|---|---|
| P1: a failed configuration apply can prune a live mock | In `src/configdoc/mod.rs`, `apply_mocks` drops definitions that cannot load before calling the store's `apply`. Applying with `prune=true` and an unreadable spec deleted the existing mock; its next lookup returned 404. The dry run reported no deletion. | Queued correctness follow-up. Preserve failed entries or block pruning on validation failure. A regression test must prove the existing mock and its persisted copy survive a failed apply and that preview agrees with execution. |
| P1: concurrent mock mutations race during persistence | `src/mocks/persist.rs::save` shares one temporary filename per mock, and `src/mocks/store.rs::saved` runs after the mutation lock is released. All 250 concurrent enable/disable requests returned 200 while 171 saves logged failures. The final saved file was valid JSON in this run; stale writes and restart behaviour need dedicated tests. | Already owned by [step 07](step-07-config-and-source-control.md). Carry this reproduction into its regression coverage: ordered writes per mock, unique temporary files, durable writes and visible save failures. Verify the final acknowledged state survives restart. |
| P2: dry runs accept configurations the real apply rejects | `src/configdoc/mod.rs::apply_settings` returns before settings validation. A numeric `console.title` produced `ok: true` in preview and failed during apply. Mock previews also lack the compilation checks used by execution. | Queued correctness follow-up. Build one validated execution plan for preview and apply, including setting types, locks, mock compilation and applicable limits. Invalid input must fail preview without mutating state. |
| P2: git previews truncate larger specs | `src/sourcecontrol/mod.rs::git` truncates stdout to 8,192 characters, including content read with `git show` by `extract`. A valid 14 KB spec became invalid JSON during a pull preview, which still reported `ok: true`. | Queued correctness follow-up. Separate diagnostic output limits from bounded, intact document reads. Test a large configuration and spec through commit, push, preview and apply with no false drift or truncated data. |
| P2: git does not retain the checked destination addresses | `SourceControl::permit` checks resolved addresses, but the git subprocess resolves independently and `git_env` permits initial HTTPS redirects. This was established by code inspection, without a live DNS-rebinding reproduction. | Already owned by [step 06](step-06-egress-and-git-transport.md). Keep its transport pinning, redirect and credential-target tests as the completion gate. |
| Uncommitted assist model management is incomplete | The new `assist_jobs` group is unregistered, the generated README endpoint table is stale, and `ui/views/manage.js` still shows the curated models rather than the loaded catalogue and per-job choices. | Already owned by [step 01](step-01-finish-assist-models.md). Register the group, regenerate the table and complete the console and parity coverage there. |

Validation at review time: 1,093 library tests passed and two failed (the settings-group and README
consistency checks above); all 14 integration/logging tests passed. The configuration and git
findings were reproduced against temporary instances and a temporary repository. The persistence
race used a temporary instance and state directory. No application fixes were made by the review.

### 8.2 Additional correctness work, after the current plan

When section 4 is complete, re-run the reproductions for the three queued findings above. Close
any finding already fixed by the earlier work with its regression evidence; otherwise implement
these in order:

1. **Safe pruning on failed input.** A document that names an existing mock but cannot load its
   definition must not turn that load failure into a deletion.
2. **Validated previews.** Preview and execution use the same validation and planned operations;
   any remaining non-atomic behaviour is explicit in the result and the user docs.
3. **Intact git document reads.** Repository documents and specs are read within their supported
   size budgets, without passing through diagnostic truncation or redaction that changes data.

Each fix starts with a failing regression test, covers the API and its CLI, console and MCP
callers where applicable, and updates the configuration/source-control documentation. Complete
this correctness work before starting the new workflows below.

### 8.3 Feature backlog, in suggested build order

These are proposed capabilities, not features shipped by 0.2.2. Split each into its own step file
when this milestone becomes active, following the five-surface rule and bounded-state rules.

| Order | Capability | User outcome and acceptance criteria |
|---|---|---|
| 1 | Instance diagnostics | Add `rustybin doctor` and a console health panel covering writable storage, durable secret-key availability, mock/config load errors, listener failures and outbound refusals. Distinguish a configured persistence path from working persistence. Explain how to resolve each problem without exposing credentials; retain the difference between operational readiness and the demo `/health` toggle. |
| 2 | Guided gateway scenarios | Let a user choose an outcome such as testing retries, rate limiting or credential injection. Prepare the required mock and faults, explain the gateway setup, send test traffic, show expected versus observed behaviour, and clean up only the resources the scenario created. No gateway-vendor-specific dependency. |
| 3 | Capture, replay and assert | Turn inspected requests into saved tests with status, header, body and timing assertions. Replay directly or through the configured gateway and export/run the tests in CI. Redact credentials, respect session/owner boundaries and outbound policy, and bound captured bodies, run duration and traffic. Extend the existing curl copy and request comparison features. |
| 4 | Scenario checkpoints and reset | Save a known baseline of the mocks, bounded stateful data, health and fault settings needed by a demo, then restore it before a run. Make reset scope explicit, protect other users' resources and secrets, and test repeatable results across consecutive runs. |
| 5 | Conflict protection for edits | Require an expected revision for mock and operation edits, with a clear conflict response and a way to reload or compare changes. Prevent two users or agents editing from the same baseline from silently overwriting each other. Cover the API, console, CLI and management MCP. |
| 6 | AI model-selection follow-through | Verify that step 01 delivered the loaded catalogue, console job pickers, endpoint/account-scoped discovery cache and model validation. Complete any remaining gaps instead of creating a second model-management implementation; document how discovery refreshes and how unsupported selections are handled. |

Suggested first follow-up release: the additional correctness fixes and instance diagnostics.
Then build guided scenarios and replayable assertions, followed by checkpoints and edit-conflict
protection. Reassess the AI row against the completed step 01 before allocating work to it.
