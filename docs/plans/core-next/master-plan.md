# Master plan: the rest of the roadmap

Planned 2026-11-07, when the owner said to build everything left and put the best possible Rustybin
instances first, leaving `rustybin-platform` for now. The steps are the "Under consideration" list of
`docs/src/roadmap.md`, ordered by what an instance gains for what it risks. Each step is built whole,
committed and pushed as it lands, under the five-surface rule. One release at the end.

## 1. Principles

- **An instance first**: what a person running Rustybin gains, before what a hosted service would.
- **Nothing that a public instance would regret**: a feature that reaches out goes through
  `egress::Egress`, a visitor gets it only when the threat model says so.
- **As in the last plans**: a failing test first, a timing change measured under the parallel suite, a
  Hurl example per route, the changelog, `docs/src/roadmap.md` kept current.

## 2. Left out

- **A platform authorization server**: the platform side, not this repository.
- **Self-service across replicas**: needs a shared store and a second instance to prove it; the demo is
  one instance by the owner's decision of 2026-10-18. Last, if at all.

## 3. Steps, order and status

The marker a session looks for is the first row that is not done.

| Step | Title | Status |
|---|---|---|
| 01 | `rustybin egress check <url>`: the dry check on every surface (`GET /_rustybin/egress/check`, `Egress::dry_check`) | done 2026-11-07 |
| 02 | A mocks-only export at version 2 | done 2026-11-07 |
| 03 | Fetching a spec from a URL on the server, for an agent (`mocks/fetch.rs`, `Purpose::SpecFetch`; `rustybin.yaml` has no `spec_url` on purpose: applying a file never reaches out) | done 2026-11-07 |
| 04 | Harness: a leader transfer on purpose, before maintenance (`Cluster::step_aside`: the leader answers as one without a quorum, so the ordinary election runs and no protocol is new) | done 2026-11-07 |
| 05 | Stateful mode for visitor mocks, with tight caps (`RUSTYBIN_MOCKS_SELF_SERVICE_STATEFUL`, off by default: the owner's decision of 2026-10-18 stands unless they turn it on) | done 2026-11-07 |
| 06 | Source control: webhooks that trigger a pull (`POST /_rustybin/git/webhook`, HMAC signed, off until `git.webhook_secret` is set) | done 2026-11-07 |
| 07 | AI mocks: Generate examples per operation, with a persona (mostly built already: persona and scope were on every surface and the dialog previews; added suggestions and Generate more) | done 2026-11-07 |
| 08 | Harness: replay captured traffic or a HAR file (`harness/import.rs`: an import into `source.requests`, not a new source, so the engine and the safety checks are unchanged; credentials removed) | done 2026-11-07 |
| 09 | MCP-level OAuth on a mock's own MCP server (`mcp.oauth`, through the same `auth::require_bearer_for` as `/mcp/protected`; not on a visitor's mock) | done 2026-11-07 |
| 10 | Harness: OpenTelemetry export (`harness/otlp.rs`: metrics of finished runs as OTLP over HTTP with JSON, no new dependency; traces are not sent, a run has no spans) | done 2026-11-07 |
| [11](step-11-source-control.md) | Source control: branches per environment, signed commits, a divergence resolved by naming a side or through a pull request, `file:` references for the harness section | done 2026-11-09 |
| [12](step-12-agent-secrets-and-sign-in.md) | Agents: secrets with an out-of-band confirmation; an interactive sign-in from the CLI | done 2026-11-09 |
| [13](step-13-harness-follow-ups.md) | Harness: scenario import, HTTP/3 targets, server-to-server chains | done 2026-11-09 |
| [14](step-14-release-0-7.md) | Release 0.7.0: steps 11 to 13 pushed, tagged, the signed image and its digest | the owner's to start, section 6 |
| [15](step-15-client-id-metadata-documents.md) | MCP inspector: client ID metadata documents (and whether the control plane accepts one) | planned 2026-11-09; part B: owner decision, section 6 |
| [16](step-16-chain-metrics.md) | Harness: a Prometheus series for chain calls | planned 2026-11-09 |
| [17](step-17-secret-request-follow-ups.md) | Secret requests: a count outside Management, a note on a decline, a user sees their own agents' requests | planned 2026-11-09 |
| [18](step-18-import-variables.md) | Harness: variables given to a scenario import | planned 2026-11-09 |
| [19](step-19-verified-pulls.md) | Source control: pulled commits verified against allowed signers | planned 2026-11-09: owner decision, section 6 |
| [20](step-20-pull-request-api.md) | Source control: a pull request opened through the git host | planned 2026-11-09: owner decision, section 6 |
| [21](step-21-chain-over-http3.md) | Harness: a chain hop over HTTP/3 | planned 2026-11-09 |
| [22](step-22-http3-image.md) | An image with HTTP/3 | planned 2026-11-09: owner decision, section 6 |
| [23](step-23-visitor-oauth.md) | OAuth sign-in for visitors and key holders | waits for the owner, section 6 |
| [24](step-24-self-service-replicas.md) | Self-service across replicas | waits for the owner, section 6 |
| [25](step-25-release-0-8.md) | Release 0.8.0: whichever of 15 to 24 are built | the owner's to start, section 6 |
| [26](step-26-open-the-demo.md) | The demo on the current release, and open to visitors | the owner's to start, section 6 |

Released as 0.6.0 after step 10. Steps 11 to 13 are built and merged, and are released when the owner says so.

## 4. Why steps 11 to 13 stopped

Each of these changes a rule the project set on purpose, or needs a call only the owner can make, so
they wait for one rather than a guess:

- **Merges inside the instance**: source control is "never a merge" by design; a diverged branch
  answers `409` with the two ways out. Changing that decides who resolves a conflict and how a
  repository writer stays limited. Branches per environment and signed commits are smaller, and
  turn on what key signs and whether a document may name a branch.
- **Secrets through an agent**: "a secret is never a tool input" is a hard rule, tested by
  `every_tool_is_fully_defined`. An out-of-band confirmation is an exception to it, and what confirms
  (a console prompt, a one-time link) is the design.
- **An interactive agent sign-in from the CLI** and **OAuth for visitors and key holders**: the second
  reopens what step 04b of the hosted demo closed on purpose.
- **Server-to-server chains**: the harness plan holds them back until the safety model covers outbound
  traffic from the server role.
- **HTTP/3 targets**: a QUIC stack in the load engine, a new dependency with a large surface, for a
  protocol few gateways under test speak.
- **Scenario import from other tools**: which tools, and how much of each one's model, is a choice
  of what to support for years.

## 5. Steps 11 to 13: the owner said build it all (2026-11-09)

The owner read section 4 and answered "build it all out", one agent per step. No single choice of
section 4 was named, so each step takes the choice that keeps the rule it touches, and anything that
could surprise an instance is off until someone turns it on. A choice the owner wants otherwise is a
change to the step, not a new plan.

**Step 11, source control**

- **Still never a textual merge.** A diverged branch keeps answering `409`. What is new is the two
  ways out as calls: a person names a side (the instance's document committed on top of the remote
  head, never a forced push; or the repository's document applied with `ApplyOptions.untrusted`), or
  the instance's side is pushed to a new branch for a pull request on the host. The automatic mode
  never resolves.
- **Branches per environment**: the branch stays the `git.branch` setting. Listing the remote's
  branches, switching with a preview that validates like the apply, and starting a new branch from
  the instance are calls. A document never names a branch and a document from git never changes
  `git.*`.
- **Signed commits**: SSH signatures with a key of their own (a secret setting, never the deploy
  key), off by default; the public key and its fingerprint are shown so the owner registers it at
  the host.
- **`file:` references for the harness section**, from the same roadmap line.

**Step 12, agents**

- **A secret is still never a tool input.** An agent asks for a secret to be set (the setting's key
  and a reason); a person with the permission to set it types the value in the console or gives it
  to the CLI on stdin. The tool answers the request's state only. Requests are bounded and expire.
  `every_tool_is_fully_defined` stays as it is.
- **The interactive sign-in from the CLI** walks the control plane's own OAuth (PKCE, a loopback
  address, the console's consent), as `rustybin login --sso` does for people. Its tokens stay
  accepted on `/_rustybin/mcp` only.
- **OAuth for visitors and key holders is not built**: it reopens step 04b of the hosted demo, and
  the demo has no visitors.

**Step 13, harness**

- **Scenario import** stays an import into `source.requests` (`harness/import.rs`), for the
  declarative formats: Postman collections (v2.1) and Artillery scripts. Scripts in a programming
  language are not run. What does not map is named in the answer; credentials are removed.
- **HTTP/3 targets** sit behind a cargo feature that is off in the default build, so the default
  binary gains no QUIC stack; the parameter is in the schema either way and a build without the
  feature refuses it with the reason.
- **Server-to-server chains**: the safety model first. A server calls on only for a request that
  carries its run's signed marker, only to a hop the scenario names and the run verified, through
  `safety.rs` (allowlist, pinning, forbidden addresses, the kill switch), with a hop count in the
  marker and a small fixed depth.

How the three were built: each by its own agent in its own worktree and branch, the step file
written first, then tests, code and docs; merged into `main` on 2026-11-09 with the full checks run
on the merge. Each step file's build notes say where the build differs from these choices and what
was left out (step 11: no verification of pulled commits' signatures; step 13: no image with
`harness-http3`).

## 6. Steps 14 to 26: everything that is left (planned 2026-11-09)

After steps 11 to 13 the owner asked what else there is to build, then for a plan of all of it in
this file. The steps come from three places: what `docs/src/roadmap.md` still has under
consideration, what the build notes of steps 11 to 13 list as not built, and what is built but not
yet released or deployed. Nothing here is built until the owner says go, a step at a time or
several at once; a step is built whole, under the five-surface rule, and pushed as it lands.

### Order

| Group | Steps | Why here |
|---|---|---|
| Ship what is built | 14 | Three steps sit on `main` unreleased; everything later builds on them |
| Small, no decision | 15 (part A), 16, 17, 18, 21 | Each finishes something a built step left, in a day or less |
| Medium, a decision each | 15 (part B), 19, 20, 22 | Each is ready to build once its row below is answered |
| Large, waiting | 23, 24 | Each reopens a decision made on purpose; the recommendation is not now |
| Ship and deploy | 25, 26 | A release of what was built, then the demo |

Steps 16, 17, 18 and 21 touch different modules and can be built side by side as steps 11 to 13
were (one agent and one worktree each; give each its own `RUSTYBIN_DOCS_PORT` and
`RUSTYBIN_UI_PORT`, expect conflicts only at the ends of the shared tables). Step 15 changes the
OAuth walk of the console and is better alone. Step 21 comes before step 22, so an image with HTTP/3
has chains over it. Steps 19 and 20 both change `src/sourcecontrol/` and go one after the other.

### Decisions for the owner

| Step | The question | Recommended |
|---|---|---|
| 14 | Push `main` and release steps 11 to 13 as 0.7.0? | Yes, before anything else is built on top |
| 15 B | May the control plane fetch a client's metadata document, a call out for a caller who has not signed in? | Build it off by default, under an egress purpose with no default allowlist; leave the mock authorization server without a fetch |
| 19 | How are a git host's own commits treated when pulls are verified? | SSH signatures only; a merge commit passes unsigned only when it is exactly the merge of two verified parents; squash and rebase merges by the host are refused and the docs say so |
| 20 | Which git hosts get a pull request through their API? | The two largest public hosts and their self-hosted variants first, behind one trait; others when asked |
| 22 | Does a published image carry a QUIC stack? | A second image tagged `<version>-http3`; the default image, which a public instance runs, stays without |
| 23 | An authorization server open to strangers on a self-service instance? | Not now: step 04b of the hosted demo closed it on purpose and the demo has no visitors |
| 24 | More than one replica for visitors? | Not until one instance is measurably full; if built, a compose spike first and PostgreSQL behind a cargo feature |
| 25 | Release 0.8.0 when the built steps are in? | Yes, one release for the group |
| 26 | Move the demo to the current release, and open it to visitors? Email leads on or off? | Move it after step 14; opening and its date are the owner's alone |

A row answered no closes its step: the row of section 3 says so with the date, and the roadmap keeps
the line under consideration with the reason.

### Left out

- **A platform authorization server for hosted instances**: the platform side, in
  `rustybin-platform`, not this repository (section 2).
- From the build notes, too small or too speculative to be steps: deleting a remote branch and
  listing local ones (step 11), a body or headers on a chain's onward call (step 13), a latency
  histogram per hop in Prometheus (the report has it). Each goes into a step above if that step
  finds it needs it.

### How a session works

As before: read this file, the step file and CLAUDE.md; a failing test first where the behaviour
exists to fail (steps 11 to 13 mostly wrote tests with the code, and step 13 then removed each
protection to see its test fail: do the first, or at least the second); then the code on every
surface, then the docs with a tested example per route; fmt, clippy on both toolchains and with each
feature, the full suite, `scripts/check-no-dashes.sh`, the docs examples, the include and link
checks and, when the console changed, `conformance/ui`. Add the step's build notes and its "For the
merge" text, set the row to done with the date, push.
