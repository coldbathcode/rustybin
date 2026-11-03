# Step 14: The audit log, and a security pass over access control

Access control, chapter 8 of 8.

## Goal

Who did what, and when, on one page: every change to the instance, every sign-in and every refusal
is recorded against a person (or the admin token, a JWT subject, a visit), searchable from every
surface. Then a review of the whole access-control build before the release.

## Model

- `src/audit.rs` (`AppState.audit`): an entry is the time, the actor (user id and name, or the
  credential kind and fingerprint, plus the agent when an agent made the call), the action (the
  route template and method, or the tool name), the target names (a mock, a user, a setting key),
  the result (status, or the refusal's reason and permission), the request id and the client
  address. Never a value, a body, a header, a password or a token. Names a user typed (mock names,
  test names) are third-party text for agents: `untrusted_*` in tool answers.
- Bounded: an in-memory ring of 10,000 entries plus, with a state directory, `<state>/audit/` as
  JSON Lines files rotated daily and kept 30 days (`audit.retention_days`, at most 365, and
  `audit.max_bytes`, default 256 MiB). The tracing audit lines stay as they are.
- What is recorded: every mutating control route and tool call, every sign-in (and failure, with
  the user name only when it exists), invitations, role and permission changes, SSO sign-ins with
  the groups that mapped, portal sign-ins and an item opened or closed, agent revocations, config applies and git pulls (with the commit), and
  every `403 permission_required`.

## Surfaces

API (`GET /_rustybin/audit` with `actor`, `action`, `target`, `since`, `until`, `result` filters and
a cursor; `/_rustybin/audit/stream` SSE; `audit.read`), console (Management, Audit: a filterable
table, live tail, an entry links to the user and the target), management MCP (`list_audit`,
`untrusted_*` for typed names), CLI (`rustybin audit list | tail`, `--json`), `rustybin.yaml` (the
`audit.*` settings; the log itself is never exported).

## The security pass

A review of steps 07 to 14 with the same method as the 0.3 review (`docs/plans/review-0.3`):
reviewers per area (the model and resolution, enforcement and the parity tables, invitations and
sign-in, SSO token checks, projects' filtering, agents, the portal's session and closed items, the audit log's completeness and what it
must never hold), each finding verified with a failing test, fixed in this step or listed for the
owner. The threat model page (`docs/src/concepts/public-instances.md`) gains "a user with a role".

## Tests

Every mutating route and tool writes exactly one entry (a test walks the catalogue and the tool
list, as the parity tests do); nothing secret in an entry (a test plants values and greps the
ring and the files); rotation and retention on an injectable clock; the filters and the cursor; the
stream.
