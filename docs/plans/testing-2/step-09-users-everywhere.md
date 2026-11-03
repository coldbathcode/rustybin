# Step 09: Users and roles on every surface, with invitations

Access control, chapter 3 of 8.

## Goal

An admin adds people, gives them roles, disables or removes them, from the console, the CLI, an
agent or `rustybin.yaml`. Nobody but its owner ever sees or types a password: people join through
a one-time invitation and set their own.

## Model

- **Invitations.** Inviting a user creates them (disabled until accepted) and answers a one-time
  link `https://<host>/ui/#/join/<token>` (32 random bytes, stored as a SHA-256, 72 hours, at most
  100 open). The link opens a page where the person sets their password (the 0.3 password rules)
  and is signed in. A reset is a new invitation; the old password stops working when it is
  accepted. The link is shown once to whoever invited (as the harness join token is), never
  stored, never logged.
- **Your own account.** Every signed-in user changes their own password (with the current one),
  display name and email, lists and ends their own sessions. The 0.3 `PUT /_rustybin/password`
  becomes this for the caller.
- **Guard rails.** The last user with the `admin` role (enabled) cannot be disabled, demoted or
  deleted; nobody changes their own roles; a custom role in use cannot be deleted; disabling a user
  ends their sessions and revokes their agents (step 12).
- **Sign-in.** `POST /_rustybin/login` takes `username` as today, looked up in the user store; the
  throttle stays per address bucket and user name; a disabled user gets the same answer as a wrong
  password.

## Surfaces

- **API**: `/_rustybin/users` (GET `users.read`, POST invites, `users.manage`),
  `/_rustybin/users/{id}` (GET, PATCH roles, display name, disabled; DELETE), `/{id}/invite` (a new
  link), `/{id}/sessions` (GET, DELETE); `/_rustybin/roles` (GET, POST), `/_rustybin/roles/{name}`
  (GET, PUT, DELETE); `/_rustybin/me` (GET, PATCH, `/me/password`, `/me/sessions`); `POST
  /_rustybin/join` (open, rate limited: the invitation token and the new password).
- **Console**: Management, Users and roles (a table of users with their roles and last sign-in,
  Invite with the link to copy, a roles table with the permission matrix, privileged roles marked);
  the account menu's My account; the `#/join/<token>` page.
- **Management MCP**: `list_users`, `invite_user` (confirmed; answers the link once), `update_user`
  (roles, disabled; confirmed when it grants a privileged role), `delete_user` (confirmed),
  `list_roles`, `save_role` (confirmed when privileged), `delete_role`. No tool takes or returns a
  password or a hash (the secret-input rule of `every_tool_is_fully_defined` covers `password`).
- **CLI**: `rustybin users list | show | invite | update | disable | enable | delete | sessions`,
  `rustybin roles list | show | save | delete`, `rustybin me | passwd`.
- **`rustybin.yaml`**: `users:` (name, display name, email, roles, disabled; never a password or a
  hash; a user the document creates joins through an invitation the apply answers) and `roles:`
  (custom roles). The export writes both; prune never removes the last admin nor the applying user.

## Documents from git

A pulled document is untrusted (0.3.1): it may not create users, change roles or define a role
that grants `users.manage` or `settings.security`; those entries are reported and kept as they
are (`REPOSITORY_REFUSED` grows a sections rule). See the decision in section 6 of the master plan.

## Tests

The invitation life (one use, expiry, the cap, a reset); the guard rails; a disabled user's
sign-in answer; `/me` for every role; the five-surface parity tests; the document round trip, the
prune rules and the untrusted refusals; the CLI commands in the live test; the console conformance
(invite, join in a second browser context, change a role, the user's view changes).

## Build notes (2026-11-03)

- **Where it is.** The model in `src/rbac/users.rs` (invitations, the `pending` flag, the guard
  rails, `update`, `delete`, `set_password`, `add_invited`, `find` by id or name), the shared logic in
  `src/rbac/service.rs` (what the routes, the tools and documents call), the routes in
  `src/rbac/api.rs` (`/_rustybin/users*`, `/roles*`, `/me`, `/me/sessions*`), the password and the
  invitation in `src/login.rs` (`PUT /_rustybin/me/password`, the 0.3 `PUT /_rustybin/password`,
  `POST /_rustybin/join`), the tools in `src/mcp/manage/users.rs`, the sections in
  `src/configdoc/mod.rs`, the console in `ui/views/manage/users.js`, `ui/views/account.js` and the
  join page in `ui/app.js` (`handleJoin`). Tests: `src/rbac/users_tests.rs`, the MCP tests at the
  end of `src/mcp/manage/tests.rs`, the CLI live test, `run_users` in `conformance/ui/console_e2e.py`,
  `docs/examples/concepts/users.hurl` and the `users` anchor of `docs/examples/cli/cli.sh`.
- **Invitations live in `users.json`** (the SHA-256 of the token, the user id, the times, who
  invited, whether it is a reset), so they survive a restart; expired ones are dropped at every
  change. The token is `rbi_` plus 43 base64url characters (32 random bytes), checked for its shape
  before it is hashed. One open invitation per user (a new one replaces it). A refused invitation
  (the cap) leaves no user behind.
- **`pending`** is a new field of `User`: invited and not joined (also disabled). An invited user
  is never enabled by a `PATCH` (`409 invited`: they join through the link); disabling one drops the
  invitation and makes them a plain disabled user (enable, then a new link is a reset). A reset of a
  disabled user is refused until they are enabled. Accepting a reset of user one writes the sign-in
  settings' `console.password_hash` (user one's password stays there, as step 07 decided); a reset of
  user one is refused while `RUSTYBIN_SETTINGS_LOCK` is on.
- **The guard rails sit in the store** (`Users::commit` refuses any change that leaves no enabled
  admin when there was one), so the API, the tools, documents and tests all meet them. Decisions where
  the plan was silent: **user one is never deleted** (`409 first_admin`; its sign-in is the instance's
  sign-in settings, and deleting it would be undone by the migration at the next start or sign-in:
  disable it instead); nobody changing their own roles is `403 own_roles` (setting the same roles is
  not a change).
- **Decision: nobody grants or manages more than they hold.** Not in the plan: a caller gives only
  roles whose permissions it holds, changes, disables, deletes, resets or signs out only users whose
  permissions it holds, and saves or deletes only custom roles within its own. Without it a custom
  role with `users.manage` could invite an admin or reset the first admin's password and take the
  account. An admin holds every permission, so it binds only such custom roles; the `403` names the
  first permission missing. Documents follow the same rule (the applying caller).
- **Disabling or deleting a user** ends their sessions (their generation moves on in the model; the
  listed sessions are forgotten through `rbac::service::ended_user`, which is the hook where step 12
  revokes the agents acting for the user). Signing a user out everywhere (`DELETE
  /_rustybin/users/{id}/sessions`) rotates their generation only.
- **Decision: the first admin's password change still signs every session out**, as in 0.4 (the
  sign-in settings rotate the instance's generation). Narrowing it to user one was tried and
  dropped: the management server's OAuth tokens carry only the instance's generation (they act for
  no user yet), so a compromised first admin changing their password would no longer end them.
  Step 12, which ties agents to a user, can narrow it. Another user's password change (`PUT
  /_rustybin/me/password`, a reset accepted) moves that user's generation only.
- **`PUT /_rustybin/password`** is now `.authenticated()`: the caller's own password (user one's goes
  to the sign-in settings and still takes `username`, other users' to the store and refuse a new
  name); a caller who is not a user (the admin token, a control JWT) changes user one's with
  `users.manage`, a body check in `BODY_CHECKS`. `PUT /_rustybin/me/password` is the same for users
  and answers `400 not_a_user` otherwise. All `/me` routes answer `400 not_a_user` for a credential
  that is not a user's (decision: a `GET` too, so the CLI's `rustybin me` says so).
- **Users exist only with the password sign-in**: `/_rustybin/users*`, `/me*` and `/join` answer `404`
  in the other modes; `/_rustybin/roles*` work in every mode (a control JWT's `rb_roles` may name a
  custom role). The export writes `users:` only with the password sign-in and `roles:` always; the
  apply reports a `users:` section elsewhere and leaves it alone.
- **`POST /_rustybin/join`** is open (`login::is_public_path`), and throttled with the sign-in's
  `Tokens::check` under the attempt key `(invitation)` (per address bucket, plus the global budget),
  so a token guess counts like a password guess. The password rules are checked before the token (a
  bad password never uses up the invitation and costs no failure); the token before the argon2 hash.
  An invalid, used, replaced or expired token is `401 invalid_invitation`. The answer is a sign-in
  token (the person is signed in) plus `joined` and `reset`.
- **Documents.** `users:` (`UserDoc`: name, display name, email, roles, disabled, `deny_unknown_fields`
  so a `password:` makes the section fail to load) and `roles:` (`CustomRole`). Roles are applied
  first, then users, then the prune of roles. A user the document creates is invited and the apply
  answers `results.users.invitations` (name, url, expires_at); one it creates disabled gets no link;
  a file loaded at startup (`apply_extra`) creates them invited without a link (`add_invited`, a log
  line says how to send one). `ApplyOptions` grew `caller` (the rules above, the applying user never
  pruned) and `base_url` (the links). An invited user exports as `disabled: false` (they will be active
  once they join), so the round trip is stable. Both sections need `users.manage`
  (`configdoc::sections_need`).
- **From git** (`ApplyOptions.untrusted`): `REPOSITORY_REFUSED_SECTIONS` is the sections rule next to
  `REPOSITORY_REFUSED`. Decision beyond the plan: besides new users, role changes and privileged
  roles, a pulled document cannot change `disabled` nor prune users (both would let a pusher lock the
  team out), nor change or prune a role that is privileged now. Display names and emails apply. Each
  refusal is a line in `problems` and the entry is kept as it is.
- **Found and fixed (a step 08 gap):** the `apply_config` tool did not check what a document's
  sections need (`settings:`, `secrets:`, `agents:`), only the REST route did. It now calls
  `configdoc::sections_need` (the REST route's `check_sections` is built on it), and both cover
  `users:` and `roles:`.
- **Tools** at the end of `TOOLS`: `list_users`, `invite_user` (always confirmed; `reset: true` for a
  user who exists), `update_user` (confirmed when it grants a privileged role), `delete_user`
  (confirmed, destructive), `list_roles`, `save_role` (confirmed when privileged), `delete_role` (not
  destructive in the MCP sense and not confirmed, as the plan lists it: a role in use is refused, so
  nobody's access changes). `list_sessions` and `end_session` take `user` (one user's sessions; `user`
  is confirmed like `all`); they cover `/_rustybin/users/{id}/sessions`. The `/me*`, `/me/password`
  and `/join` routes are `Excluded` in `COVERAGE` with reasons (a person's own account until agents
  act for a user in step 12; a password passes through nobody but its owner). The secret-input rule of
  `every_tool_is_fully_defined` now refuses any key containing `password` or `hash`. The admin's tool
  list is 132 (`users_and_roles_tools_invite_confirm_and_never_carry_a_password` counts it;
  `docs/src/coding-agents.md` says so, and 113 without the harness tools). The ceilings treat the new
  tools as 0.4's admin tools (`OLD_RIGHTS`).
- **CLI**: `users list | invite | show | update | disable | enable | sessions | delete` (a user is an
  id or a sign-in name; `invite --reset` is the new link, `sessions --end` signs out), `roles list |
  show | save | delete`, `me` (`update`, `sessions [--end-all]`, `end-session`) and `passwd` (`PUT
  /_rustybin/me/password`, the profile keeps the fresh token). `POST /_rustybin/join` is `UNCOVERED`
  (a browser flow). `password change` stays (the 0.3 route).
- **Who can call what** (`src/rbac/access_table.rs`): the new routes are listed in `NEW_SINCE_0_4`
  (no 0.4 answer to compare; their declarations are checked by the other parity tests). One
  deliberate change: `PUT /_rustybin/password` is reachable by any signed-in caller (the four
  `DELIBERATE` rows for the `console`, `inspector` and `harness` scopes and nobody on an open control
  plane); for those callers, who are not users, the handler still requires `users.manage`, so what
  they can do is unchanged.
- **Console**: Management, **Users and roles** (`TABS`), the account menu (a `details` menu on the
  top bar's name: My account, the sign-in settings, Sign out; `#signed-in` is its summary), **My
  account** (a hidden view, `#/account`), and the join page, shown instead of the console when the
  hash is `#/join/<token>` (the token leaves the address bar at once; after the join the page reloads
  on `#/account`). Dates days ahead (expiries, sign-ins) show as the browser's date and time.
- **For steps 10 to 13**: SSO users are `Source::Sso` and should never get an invitation or a store
  password (`invite`, `set_password` do not check the source yet); step 12 hooks
  `rbac::service::ended_user` and the agents' `Credential::Console` (by user name today) should move
  to the user id; step 11's project roles will need the guard rails' "last admin" to stay instance
  wide; step 13's portal sign-in reuses the user store (portal readers join through the same
  invitations).
