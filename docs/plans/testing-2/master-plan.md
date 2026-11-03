# Master plan: testing workflows 2 and access control

Planned 2026-11-02, after 0.3.0, from the first candidate under consideration in
`docs/src/roadmap.md` and the build notes of the reliability plan; access control (steps 07 to 14)
added the same day after 0.3.1, at the owner's request; open and closed APIs on the portal (step
13) added on 2026-11-03. Two releases (decided 2026-11-03, the owner left the timing to the build): 0.4.0 after
step 06 (the testing workflows), 0.5.0 after step 15 (access control). Built only when the owner says go, one step
at a time, each pushed as it lands.

## 1. What this milestone is

0.3.0 made an instance a test bench: gateway scenarios, saved tests that replay in CI,
checkpoints, edits that cannot overwrite each other. This milestone makes those workflows run
without a person at the console: saved tests on a schedule with a report history and a failure
notice, more gateway scenarios (circuit breakers, request size limits, CORS, header
transformation), checkpoints kept in git next to `rustybin.yaml`, and a compare view when an edit
is refused as stale.

Then it makes an instance safe to share with a team (steps 07 to 14): several users, each with
roles made of named permissions (five built-in roles, custom ones), invitations so nobody but its
owner sees a password, single sign-on with any OpenID Connect provider whose groups pick the role,
projects so teams share one instance with roles on their part of it, agents that act for a user and
never exceed them, APIs on the portal that are open to anyone or closed behind a portal sign-in
(user name and password), and an audit log of who did what. Every route and every tool declares the
permission it needs, checked in one place.

## 2. Principles

- **A failing test first**, as in the last two plans.
- **The five-surface rule**: the API (catalogue and OpenAPI), the console, the management MCP, the
  CLI and `rustybin.yaml` (or a written reason) in the same step, the parity tests green.
- **Bounded state, bounded work**: a schedule list, a report history and every outbound call have a
  cap and a lifetime; outbound calls go through `egress::Egress` under a `Purpose`.
- **Nothing reaches what it did not create**, as before: a scheduled run uses the operator's
  saved tests only; a scenario cleans up only its own resources.
- **Deny by default** (access control): a control route or tool without a declared permission fails
  a parity test; an unknown permission or role name is refused; a `403` names the permission that
  is missing, never what the caller holds.
- **Permissions are resolved per request** from the user, so a role change, a disable or a removal
  applies to the next request, not at a token's expiry.
- **Passwords pass through nobody but their owner**: invitations, never an admin typing one; no
  tool, document or log line ever holds a password or a hash.
- **Break glass stays**: the admin token is every permission and works when SSO or the user store
  does not.
- **Project hard rules**: no em or en dashes, no panics on request paths, vendor neutral (generic
  OpenID Connect, tested against Rustybin's own demo identity provider).

## 3. What 0.3.0 left

| From | What |
|---|---|
| Saved tests (0.3 step 06) | runs are manual; the last report is gone with the answer |
| Gateway scenarios (0.3 step 05) | five outcomes; circuit breakers, size limits, CORS and header rules are common asks |
| Checkpoints (0.3 step 07) | kept in `<state>/checkpoints/` only; a team sharing a repository cannot share them |
| Edit conflicts (0.3 step 08) | a `409` names what changed and offers a reload; it does not show the difference |
| Control plane (0.2 step 02, 0.3.1 step 04) | one console user with every right; four scopes (`admin`, `console`, `inspector`, `harness`) as booleans; 73 yes/no admin checks in 22 files |
| Management MCP (0.2 step 08) | seven tool rights and four agent ceilings, separate from the scopes; an agent is tied to a credential, not a person |
| Dev portal (0.2 step 07) | `portal.auth` is one switch for the whole portal; its `password` mode signs in with the console's admin account; a mock's reference page and spec are public whatever the switch says |
| Audit | tracing lines without arguments; no record of who did what that a person can search |

## 4. Steps, order and status

The marker a session looks for is the first row that is not done.

| Step | Title | Needs | Status |
|---|---|---|---|
| [01](step-01-run-history.md) | A history of saved-test runs | | done 2026-11-03 |
| [02](step-02-schedules.md) | Saved tests on a schedule, with a failure notice | 01 | done 2026-11-03 |
| [03](step-03-scenarios-resilience.md) | Scenarios: circuit breakers and request size limits | | done 2026-11-03 |
| [04](step-04-scenarios-http.md) | Scenarios: CORS and header transformation | | done 2026-11-03 |
| [05](step-05-checkpoints-in-git.md) | Checkpoints in the configuration repository | | done 2026-11-03 |
| [06](step-06-conflict-compare.md) | A compare view for a stale edit | | done 2026-11-03 |
| 06r | Release 0.4.0: the testing workflows (the step 15 recipe for steps 01 to 06) | 01 to 06 | done 2026-11-03 (`v0.4.0`, image `sha256:db4d1493d612...`) |
| [07](step-07-users-and-roles.md) | Access control 1: users, roles and permissions (the model) | | done 2026-11-03 |
| [08](step-08-permissions-everywhere.md) | Access control 2: every route and tool checks a permission | 07 | done 2026-11-03 |
| [09](step-09-users-everywhere.md) | Access control 3: users and roles on every surface, invitations | 08 | |
| [10](step-10-single-sign-on.md) | Access control 4: single sign-on with OpenID Connect | 09 | |
| [11](step-11-projects.md) | Access control 5: projects, roles on part of an instance | 09 | |
| [12](step-12-agents-for-users.md) | Access control 6: agents act for a user | 09 | |
| [13](step-13-portal-access.md) | Access control 7: open and closed APIs on the portal | 09 | |
| [14](step-14-audit-log.md) | Access control 8: the audit log and a security pass | 07 to 13 | |
| [15](step-15-release.md) | Docs and release 0.5.0 (access control) | all | |

Steps 01 to 06 and 07 to 14 are independent tracks; a testing step built after step 08 declares
its routes' permissions as it lands, one built before is covered by step 08's sweep. Steps 10, 11,
12 and 13 may run in parallel once 09 is done.

## 5. How a session works

As in the reliability plan: read this file, the step file and CLAUDE.md; tests first, then the
code, then the docs (a user page with its "On every surface" table and a Hurl example per route);
run fmt, clippy (also with `rustup run stable`, which CI uses), the full suite under a watchdog,
`scripts/check-no-dashes.sh`, the docs examples and, when the console changed, `conformance/ui`.
Add the step's build notes, set the row to done with the date, push.

## 6. Decisions for the owner

- **The failure notice** (step 02): a webhook (a new egress purpose, `notify`, no default) is the
  plan; an email needs a mail service the instance does not have. Say if a webhook is not enough.
- **Schedules on a cluster**: a schedule runs on the instance that holds it; several replicas
  would each run it. The plan documents this rather than electing a runner.
- **Checkpoints in git** (step 05) hold mock data, which may be test data a team does not want in a
  repository: they are written only on an explicit commit, never by the automatic mode.
- **The built-in roles** (step 07): `viewer`, `tester`, `editor`, `admin` and `portal_reader`, with the permission sets
  in the step file. Say if the team needs another split (for example an `operator` who changes
  settings but not mocks) before step 07; custom roles cover the rest afterwards.
- **Users from git** (step 09): the plan refuses users, role changes and privileged roles from a
  pulled document (anyone who can push to the repository would otherwise make themselves admin).
  The alternative is a setting that trusts the repository for users; say if a team manages its
  users as code.
- **Projects in 0.5.0** (step 11): the largest access-control step. It can move to a later release
  without blocking the others; say if one team per instance is enough for now.
- **One release or two**, decided 2026-11-03: two. 0.4.0 ships the testing workflows after step
  06; 0.5.0 ships access control after step 15.
- **Closed APIs on the portal** (step 13), decided 2026-11-03: a signed-out visitor sees a closed
  API with a lock; its docs and calls are seen but not used until the reader signs in and gets
  the credentials (Try it, demo credentials and collections locked; a closed mock has its mocked
  security on, so a call without the credentials is refused); no 0.3.x point release, it ships
  with access control (0.5.0).
- **The audit log's home** (step 14): files in the state directory with retention. A webhook or a
  syslog target for a central log is not in the plan; say if one is needed.
