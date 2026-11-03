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

## Security pass build notes (area A)

Sign-in, invitations, single sign-on, and the model and its enforcement: 18
findings from two read-only reviews of `1e55519` (10 on sign-in and single sign-on,
A1 to A10; 8 on the model and enforcement, M1 to M8). Each has a test of the
behaviour it asks for; every one reproduced (traced on `1e55519`, whose code does
what the finding says, so the test's assertions fail there), and every one is fixed.
None deferred, none not reproduced.

- A1 (high) a name the provider lets people choose links a user added ahead: fixed,
  `sso::tests::a_name_alone_never_links_a_user_created_ahead`. A user added ahead
  links only through the email the provider verified (`email_verified: true`, equal
  to the user's email, or to the name of a user without one); a new user's name
  comes from the email only when it is verified.
- A2 (medium) sign-in timing tells a real user from an unknown name: fixed,
  `login::tests::an_unknown_name_takes_a_hash_permit_like_a_real_user`. Every path
  without a hash runs one dummy argon2id verify under a hash permit (a process-wide
  hash made once); `argon2` and `blake2` build with `opt-level = 3` in dev so tests
  that sign in stay fast.
- A3 (medium) a person removed at the provider keeps their agents through OAuth
  refresh tokens: fixed, `control_oauth::tests::an_sso_users_oauth_tokens_need_a_recent_sso_sign_in`
  and `sso::tests::a_single_sign_on_vouches_for_twelve_hours`. `SsoLink.vouched_at`
  (set at every verified single sign-on); `token_user` refuses an access or refresh
  token acting for a single sign-on user vouched for more than 12 hours ago
  (`SSO_VOUCH_SECS`, the console token's life). Sign-in tokens already last 12 hours.
- A4 (medium) anyone locks everyone out through the global failure budget: fixed,
  `login::tests::failures_at_other_names_never_lock_user_one_out` and
  `login::tests::one_client_guessing_names_never_blocks_user_one`. The budget is per
  user name (`NAME_FAILURES_PER_WINDOW`, 20 a minute), the invitation and single
  sign-on keys are lanes of their own, and the bounded map evicts the name with the
  fewest failures first (junk names never reset the count of a name under attack).
- A5 (low-medium) the open start fills the sign-in table: fixed,
  `sso::tests::a_full_sign_in_table_drops_the_busiest_address_first`. A full table
  drops the oldest sign-in of the busiest address; the per-address cap is checked
  again at insert.
- A6 (low-medium) one user's sign-ins push another's session out of the list: fixed,
  `login::tests::one_user_signing_in_never_pushes_another_out_of_the_list`. At most
  16 listed sessions per user and 4096 in all; an evicted session is ended through a
  per-user floor (`rb_ms`, a unique millisecond issue time, on new sign-in tokens),
  never through the shared denylist, so floods cannot push out others' revocations.
- A7 (low) the CLI's loopback listener takes the first callback from anyone: fixed,
  `cli::tests::the_loopback_listener_waits_for_its_own_callback`. A random
  `cli_state` in `return_to`'s query, checked in constant time; other requests get
  `400` and the listener keeps waiting; errors are printed through
  `logging::printable`.
- A8 (low) a first admin's reset link is burnt when the settings lock refuses the
  password: fixed, `login::tests::a_locked_first_admin_reset_keeps_the_link`
  (preflight with `Settings::writable(HASH_KEY)` before the invitation is used).
- A9 (low) discovery takes `http` endpoints under an `https` issuer: fixed,
  `sso::tests::discovery_endpoints_are_https_under_an_https_issuer`.
- A10 (low) the refusal says a password account has the name: fixed,
  `sso::tests::account_refusals_tell_nothing_about_the_name`. `name_taken`,
  `other_identity`, `not_provisioned` and the new `not_linked` of the store all
  reach the browser as `not_linked`, the detail in the `rustybin::sso` log.
- M1 (high) `update_settings` skips the security groups: fixed,
  `mcp::manage::tests::update_settings_tool_needs_settings_security` and the parity
  test `mcp::manage::tests::update_settings_refuses_every_key_rest_refuses` (every
  non-secret key, a `settings.write` caller: REST refuses exactly when the tool
  does, naming the same permission). Both call `settings::keys_need` and
  `settings::values_need`; the confirmation also covers `proxy.*`,
  `assist.provider` and `assist.base_url`.
- M2 (medium-high) `git.*` needs only `settings.write`: fixed,
  `rbac::security_tests::git_settings_need_git_write`. Decision: `git.write` (the
  permission that already says "configure source control"), not `settings.security`.
- M3 (medium) the sessions routes skip "manage only users whose permissions you
  hold": fixed, `rbac::security_tests::ending_a_session_needs_what_its_user_holds`.
  `rbac::service::end_session` (the session's user: `within` and
  `within_bindings`) and `end_all_sessions` (every permission), shared by REST and
  `end_session`.
- M4 (medium) `config.export` returns the user directory: fixed,
  `rbac::security_tests::an_export_without_users_read_has_no_users` and
  `mcp::manage::tests::export_config_without_users_read_has_no_users`.
  `configdoc::export_for` leaves out `users:`, `roles:` (also `users.read` behind
  `GET /_rustybin/roles`) and `projects:`. The automatic git mode still exports the
  whole instance (it needs `git.write` to set up).
- M5 (medium) the self-service bypass voids declared permissions: fixed,
  `rbac::security_tests::self_service_holds_team_callers_to_the_route_permission`.
  `check_route` lets through only a caller acting as a visitor (anonymous, an API
  key, or a manage token sent); the route's permission goes on to the per-mock rules
  (`RouteNeed`), and `Svc::route_need_on` holds it on instance mocks except reading
  them outside projects (the 0.4 visitor read).
- M6 (medium) a document from git widens a non-privileged role: fixed,
  `rbac::security_tests::a_pull_never_widens_a_role`. Decision: a pulled role never
  gains a permission; a new role is created only when `sso.role_map` and
  `sso.default_roles` do not name it (nobody else can hold a new role: git never
  changes a user's roles or a project's members), so restoring an instance from its
  repository keeps its custom roles.
- M7 (low-medium) `settings.security` maps a group to `admin`: fixed,
  `rbac::security_tests::the_sso_role_map_grants_only_what_the_setter_holds`
  (`sso::grants_need` through `settings::values_need`: REST, the tool and a
  document's `settings:`).
- M8 (low, hardening) an uncatalogued control template runs unchecked: fixed,
  `control_auth::tests::an_uncatalogued_control_route_is_refused` (`403
  permission_required`, `permission: null`, for every credential).

Who can call what changed: `DELETE /_rustybin/sessions` now needs every permission
(it needed `users.manage`); `DELETE /_rustybin/sessions/{id}` needs the session
user's permissions too; the `git.*` settings need `git.write`; an `editor`'s export
carries no users, roles or projects; on a self-service instance a user or control
JWT without a route's permission is refused where 0.4 let it through, and a visitor
no longer reads an instance mock's traffic or data; `update_settings` needs what
REST needs.
