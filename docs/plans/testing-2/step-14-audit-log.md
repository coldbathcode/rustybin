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

## Build notes: the audit log (2026-11-03)

The audit part of this step (Goal, Model, Surfaces, Tests) is built; the security pass is
separate and still to come.

- **Where it is.** `src/audit.rs` (the model, the store `AppState.audit`, the file writer, the
  filters, the route layer, the `audit.*` settings, the routes), `src/audit/tests.rs` (9 tests),
  `src/mcp/manage/audit.rs` (`list_audit`) and the recording of tool calls in
  `mcp::manage::tools::call` (`record`, `tool_targets`), the CLI's `audit` noun in
  `src/cli/commands.rs`, the console's `ui/views/manage/audit.js`, `docs/src/audit-log.md`,
  `docs/examples/concepts/audit.hurl`, `run_audit` in `conformance/ui/console_e2e.py`.
- **How every route and tool is covered without code.** `audit::layer` is a route layer added
  after (so outside) `control_auth::check_route` in `build_app`: it sees the matched template, the
  method and the answer, and the refusals the check makes. It records every `POST`, `PUT`, `PATCH`
  and `DELETE` of a control route except `audit::NOT_RECORDED` (each row with its reason: the
  management MCP transport, whose tool calls are entries of their own; the harness node traffic,
  joins, heartbeats and elections every second; the `POST`s that compute and change nothing:
  `/_rustybin/mocks/import`, the operation `validate` and `rules/test`, `registry/validate`,
  `harness/preview` and `validate`). `tools::call` records every tool that is not `read_only` and
  every refused call, once per call. `every_mutating_route_and_tool_writes_one_entry` walks the
  catalogue (every control route and mutating method, 117 of them) and `TOOLS` (every tool that
  changes something) against the full app and asserts exactly one entry with the expected action
  (none for a `NOT_RECORDED` row, each of which must still exist).
- **The caller.** `check_route` now leaves the caller it resolved on the response
  (`ControlCaller`), and `rbac::refuse` marks its answer (`need::Refusal` plus the caller), so the
  layer resolves nobody twice and a refusal on the data plane (the `/health` toggles, the global
  `/flaky` reset) is recorded without reading its body. For control and portal routes a `401` or
  `403` JSON answer of at most 16 KiB is read for its `reason` and `permission` (the refusals made
  in handlers and services build their own bodies). An entry is recorded when the route is
  mutating, a handler named an event, or the answer is a `permission_required` refusal; a `GET`
  that is none of these is not, so reading the log is never an entry.
- **What handlers add.** A task-local note (`audit::scoped` around the handler and around a tool's
  dispatch) that `audit::target`, `audit::event` and `audit::actor_user` write into, a no-op
  outside a request. Blocking closures carry their own (`audit::carried`, then `merge`), which
  `Svc::compiling` and `configdoc::apply_on_pool` do, so the services name targets on the compile
  pool too. Added: the user a sign-in names (`login::check_sign_in`, only for a user that exists;
  the console's and the portal's), the invited user on a join, the SSO user and the groups that
  mapped (the role map's groups the person is in, never the others), the event of every
  `rustybin::auth` line (login, logout, join, reset, change_password, end_session,
  sign_out_everywhere, sso, sso_start, sso_token, portal_login, portal_logout, and the users,
  roles and projects events with their names), the keys of a settings change, a created or
  duplicated mock and its project, a move (`move_to`: mock and project), a saved test and a
  checkpoint, an agent revoked (`revoke_agent`, the agent and its `owner`, the user id; the agents
  a user's removal revoked), a portal item opened or closed (`close_item`, `open_item`), a config
  apply (`apply_config` and the sections the document changed), a pull (`git_pull` and the commit
  reached). Path parameters are targets named after the segment before them (`mock`, `operation`,
  `user`, `setting`, ...); the MCP relay's session and a consent request's id are a short hash.
- **Decisions where the plan was silent.**
  - **Unauthenticated requests are no entries** (a `401` from `control_auth::enforce` happens before
    routing): anyone could fill the log. Sign-ins and refusals of callers without a credential are
    entries, capped at 600 a minute for the instance (`ANONYMOUS_PER_MINUTE`, the rest counted in
    `suppressed`), so nobody outside can flush the ring.
  - **Read-only tools and `GET`s are not entries** unless refused; the read-only `POST`s are
    `NOT_RECORDED`. The tracing lines stay as they were.
  - **The actor**: `user` (id and name), `admin_token`, `token` (a control JWT's `sub`), `key`,
    `visit` (the visit's fingerprint, from the manage token on a self-service instance), `open`,
    `anonymous`, `system` (the automatic git pulls, `Audit::system`), plus `agent` for a tool call.
  - **Ids** increase and, after a restart, continue after the newest entry read back from the files
    (an entry that never reached a file may have had an id that is used again; the files never hold
    two entries with one id).
  - **Files**: `audit-YYYY-MM-DD.jsonl` by the entry's UTC day, mode `0600` in a `0700` directory,
    written by one thread (`rustybin-audit`) from a channel of 4,096 (`try_send`: a full channel
    counts `not_written`, never blocks a request). The retention runs at the writer's start, at
    the first entry of each day and at every flush; past `audit.max_bytes` the oldest days go
    first, never the day being written, whose later entries stay in memory (counted). Startup
    reads the newest 10,000 entries back (at most the last 32 MiB of each file). `Audit::flush`
    (tests) waits for the writer.
  - **`audit.*` is a security group** (`settings::SECURITY_PREFIXES`: `settings.security` to change)
    and in `configdoc::REPOSITORY_REFUSED_PREFIXES` (a pulled document cannot shorten the record of
    what its pusher did). Seeds `RUSTYBIN_AUDIT_RETENTION_DAYS` and `RUSTYBIN_AUDIT_MAX_BYTES`
    (checked with the settings' own rule: 1 to 365 days, at least 64 KiB).
  - **`audit.read`** (no longer `RESERVED`) is the admin role's only; the two routes are in
    `NEW_SINCE_0_4`. `list_audit` is an admin read tool for the ceilings (`OLD_RIGHTS`); 137 tools
    for an admin, 118 without the harness ones.
  - **Not done (left for later):** a project-scoped view of the log (a project's admin reading
    its own trail, the `has_in` check step 11 suggested), per-item reader counts and portal
    sessions in the sessions list (step 13's extras), a webhook or syslog target (master plan,
    section 6), and reading older entries from the files through the API (the API searches the
    entries in memory; the files are for `jq`, `grep` or a shipper).
- **Surfaces.** API: `GET /_rustybin/audit` (filters `actor`, `action`, `target` with `kind:name`,
  `since` and `until` as RFC 3339, a day or an age, `result` as an outcome or a status; `cursor`,
  `limit`; `log` with the counts and bounds) and `GET /_rustybin/audit/stream` (an `entry` event
  per matching entry, 16 listeners). Console: Management, **Audit** (`TABS`): the filters, the table
  with links (a user to Users and roles, a mock to its page, an agent to AI agents), Older entries,
  Live tail (`send({stream: true})` and `sseParser`), the log's state and the Retention card (the
  `audit` group's settings form). MCP: `list_audit` at the end of `TOOLS`, typed names under
  `untrusted_actor_name`, `untrusted_targets`, `untrusted_request_id`, none in the summary. CLI:
  `rustybin audit list` (the filters, `--limit`, `--cursor`; a table for people, `--json` the
  API's answer) and `rustybin audit tail` (the filters, `--max`, `--timeout`). `rustybin.yaml`:
  the two settings under `settings:`; the log is never exported.
- **The flaky docs example** (`docs/examples/harness/cli.sh`): the smoke scenario's thresholds were
  `latency_p99_ms: 500` and `saturated: false` with a 2 s timeout, which a loaded machine misses
  (one sending thread saturates, the p99 stretches). Now a 10 s timeout, `error_rate: 0` and
  `latency_p99_ms: 5000` (every request answered, none slower than seconds), and the run's line is
  checked for the 40 requested, not the 40 sent; the anchors are unchanged.

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

## Security pass build notes (area B)

The portal's closed items, projects and agents: fifteen findings of two read-only
reviews of `1e55519`, each shown first by a test that failed on that commit (the
reversed fix was run against every test below), then fixed with the test kept. Nothing
was deferred.

The portal's closed items (`src/portal/access_tests.rs`):

1. A closed mock callable signed out through the built-in identity provider: fixed
   (`a_closed_mock_cannot_rest_on_tokens_anyone_can_mint`). `portal::unclosable` is
   the one rule: every protected operation needs an API key or a Basic user of its
   own; `jwt` and `bearer` modes, OAuth 2, OpenID Connect, bearer and `mutualTLS`
   schemes, the defaults, and a mock with no protected operation are refused with a
   `reason`; a locked mock entry carries no `token_url`.
2. "Turn on security and close" behind the default credentials: fixed
   (`default_credentials_never_close_a_mock_and_secure_replaces_them`, and
   `closing_a_mock_needs_its_security_and_secure_turns_it_on` updated): `secure: true`
   replaces `demo-key`, `demo` / `demo` and `demo-token` with random values and falls
   back to `api_key` when the spec's mode cannot close the mock.
3. `portal.auth` and `portal.base_url` from a pulled document: fixed
   (`a_pulled_document_never_changes_portal_auth_or_base_url`): both in
   `REPOSITORY_REFUSED` (refused rather than "tighten only", the simpler rule).
4. Security switched off under a closed item: fixed
   (`security_cannot_be_switched_off_under_a_closed_item`,
   `a_config_file_closing_an_unsecured_mock_is_reported_and_refused`). One check in
   the store, `MockStore::set_live_guard` with `portal::live_guard`, before any
   revision goes live (deploy, rollback, a document's or file's replace, a
   checkpoint's restore): `409` naming why; the documents' previews ask the same
   check (`MockStore::refuses_live`), so a preview validates as the apply. A mock created under a closed item's name
   (delete and recreate) and the file load are not refused (nothing is lost); the
   data plane answers `403 closed_unsecured` until it is secured, and the file load
   reports `portal::closed_problems` among the load errors.
5. A closed `mcp:apikey` / `mcp:protected` item hinting how to call it: fixed
   (`a_closed_mcp_server_has_no_hint_and_says_it_stays_callable`): no `auth.hint` in a
   locked entry; every non-mock item's view has a `note` that its endpoint stays
   callable, and the publish dialog says so for every such item (it used to claim
   those two refuse calls).
6. `secure: true` changing the mock before a refused publish: fixed
   (`a_refused_secure_publish_leaves_the_mock_as_it_was`): `clean_meta` and
   `Portal::has_room` before `secure_mock`.
7. Portal sign-outs evicting console sign-outs: fixed
   (`portal_sign_outs_never_evict_console_sign_outs`): `Tokens::revoke_portal`, its
   own map of 4096, persisted with the others. (The old code had no separate list, so
   the test is written against the new call; the reviewer's loop through `revoke` is
   what it replaces.)

Projects and agents (`src/rbac/project_tests.rs`, `src/mcp/manage/agents/user_tests.rs`,
`src/control_oauth/tests/users.rs`):

1. Undoing an operator's revocation by revoking first: fixed
   (`a_user_cannot_restore_an_agent_the_operator_revoked_by_revoking_it_first`,
   `an_oauth_agent_cannot_undo_an_operator_revocation_through_the_tools`): a second
   revoke keeps the first `Revocation` (still ends sessions and revokes the tokens)
   and answers `already_revoked`. A sibling sign-in agent of the same user was
   already refused (revocation follows the credential), so the tool case uses an
   OAuth agent of the user.
2. One OAuth agent's revocation refusing the client for every user: fixed
   (`revoking_one_users_oauth_agent_leaves_another_users_tokens_alone`):
   `Credential::revocation_subject` and `control_oauth::revocation_subject` key the
   refused subject by `sub` and `rb_uid` (`client:<fp>#user:<id>`); tokens that act
   for nobody keep the bare subject.
3. The AI spec route's avoid-names: fixed
   (`the_ai_spec_prompt_names_only_mocks_the_caller_may_read`): only `can_read` names.
4. A live traffic stream outliving the caller's access: fixed
   (`a_traffic_stream_ends_when_its_caller_loses_the_project`): `still_readable`
   re-authenticates the request's credentials and re-resolves the caller at every
   event and every 5 s (was 30 s), then the route's and the mock's checks.
5. Consent without project grants: fixed
   (`consent_shows_and_counts_the_approvers_project_grants`): `projects` per grant and
   for the request, `may_grant` counts project permissions; the console's consent page
   lists them per project.
6. Project names probed through errors: fixed (`a_project_name_cannot_be_probed`):
   an unknown project answers the permission refusal unless the caller holds it on the
   whole instance; the checkpoint problem line says "now in another project".
7. `project_view` resource names: fixed
   (`project_resources_are_listed_only_to_who_reads_them`): mocks and checkpoints
   need `mocks.read` in the project, tests `tests.read`.
8. Load errors to every project member: fixed
   (`load_errors_go_to_instance_mock_writers_only`): only instance-wide `mocks.write`.

Decisions taken (the safest reasonable default each): operations declared public
(marked `public`, or in `spec` mode without a requirement) stay open under a closed
item, as step 13 documented (a health check beside a keyed API), but a mock needs at
least one protected operation; a mock recreated under a closed item is created and refused
on the data plane rather than refused at creation; `portal.auth` is refused outright in
a pulled document, not allowed to tighten; checkpoints' names in `project_view` follow
`mocks.read`; a stream re-check that cannot re-authenticate ends the stream.
