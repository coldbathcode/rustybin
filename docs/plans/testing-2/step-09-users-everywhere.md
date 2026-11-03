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
