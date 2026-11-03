# Users and roles

An instance with the password sign-in (`RUSTYBIN_CONTROL_AUTH=password`, the
default) is shared by a team: an admin adds people, gives them roles, disables or
removes them. **Nobody but its owner ever sees or types a password**: people join
through a one-time invitation link and choose their own, and change it themselves.
With [single sign-on](sso.md) people sign in with the team's identity provider
instead: they have no password here, and the provider's groups give their roles.

A role is a set of named [permissions](concepts/control-plane-security.md#permissions).
Five roles are built in and never change; custom roles pick any permissions.

| Role | For | Permissions |
|---|---|---|
| `viewer` | Looking around: the console read-only | every `*.read` except `traffic.read`, `audit.read` and `users.read` |
| `tester` | Running what others built | `viewer` plus `traffic.read`, `tests.run`, `scenarios.run`, `harness.read`, `harness.run`, `assist.use`, `mcp.inspect` |
| `editor` | Building mocks and tests | `tester` plus every `mocks.*`, `tests.write`, `checkpoints.*`, `assist.jobs`, `portal.publish`, `registry.write`, `config.export`, `git.read`, `traffic.clear` |
| `admin` | Everything | every permission |
| `portal_reader` | Reading the API portal's closed items | `portal.read` |

A role that grants `users.manage` or `settings.security` is **privileged** (it can
change who may do what) and is marked so in every view. The first admin ("user
one") is the console user of the sign-in settings (`console.user`,
`RUSTYBIN_CONSOLE_USER`): its password stays in `console.password_hash` and the
environment, as before.

## Invite someone

`POST /_rustybin/users` (`users.manage`) creates the user, **invited** (disabled
until they accept), and answers the link `https://<host>/ui/#/join/<token>` once (for
a user whose only role is `portal_reader`, the [API portal's](portal.md#portal-logins)
join page, `https://<host>/portal/#/join/<token>`, so a tester never sees the
console):

```hurl
{{#include ../examples/concepts/users.hurl:invite}}
```

The token is 32 random bytes, kept only as a SHA-256: it is in this answer and
nowhere else (never stored, never logged, never in a list). It works once, for 72
hours; at most 100 invitations are open at once. The list shows that an invitation
is open, never its link:

```hurl
{{#include ../examples/concepts/users.hurl:list}}
```

The person opens the link, chooses a password (8 to 256 characters, not the
default) and is signed in. The page is `POST /_rustybin/join`, open like the sign-in
and rate limited the same way (5 failures per client address and minute, 100 from
every address together):

```hurl
{{#include ../examples/concepts/users.hurl:join}}
```

## Your own account

Every signed-in user reaches `/_rustybin/me`: who they are, their roles and
permissions, their display name and email (`PATCH`), their password (`PUT
/_rustybin/me/password`, with the current one) and their sign-in sessions (`GET`,
`DELETE /_rustybin/me/sessions`, `DELETE /_rustybin/me/sessions/{id}`). A password
change ends the user's other sessions and answers a fresh token (the first admin's, the
instance's sign-in setting, ends every session, as in 0.4). The 0.3 route `PUT
/_rustybin/password` is the same for the caller (and still takes the first admin's
new user name); the admin token or a control JWT, which are not users, change the
first admin's there only with `users.manage`.

```hurl
{{#include ../examples/concepts/users.hurl:me}}
```

```hurl
{{#include ../examples/concepts/users.hurl:my_password}}
```

## Roles

`GET /_rustybin/roles` (`users.read`) lists every role with its permissions, whether
it is privileged and how many users hold it, plus the permission table (name, group,
summary) the console draws its matrix from. `PUT /_rustybin/roles/{name}` creates or
changes a custom role, `POST /_rustybin/roles` creates one (409 when the name is
taken), `DELETE` removes one nobody holds. A name has 1 to 40 lower case letters,
digits, `_` and `-`; at most 50 custom roles. Roles work in every control-auth mode:
a [control JWT](concepts/control-plane-security.md#signed-access-tokens-jwt-mode)'s
`rb_roles` may name a custom role.

```hurl
{{#include ../examples/concepts/users.hurl:roles}}
```

A user's roles change with `PATCH /_rustybin/users/{id}` (an id or the sign-in
name); permissions are resolved at every request, so the change applies to the
user's next one:

```hurl
{{#include ../examples/concepts/users.hurl:change_roles}}
```

## The guard rails

- The last enabled user with the `admin` role is never disabled, demoted or deleted
  (`409 last_admin`).
- Nobody changes their own roles (`403 own_roles`): ask someone else who manages
  users.
- The first admin is never deleted (`409 first_admin`): its sign-in is the
  instance's sign-in settings; disable it instead.
- A custom role somebody holds is not deleted (`409 role_in_use`); a built-in role
  never changes (`400 built_in`).
- **Nobody grants more than they hold**: a caller gives a user only roles whose
  permissions it holds, manages (changes, disables, deletes, resets) only users whose
  permissions it holds, and saves only roles within its own. An admin holds every
  permission; this binds a custom role with `users.manage`. The `403` names the first
  permission missing.
- An invited user joins through the invitation, never by being enabled (`409
  invited`).
- In a [project](projects.md) the same rules hold for a user's roles there, and the
  last enabled admin is counted instance-wide: a project's `admin` role never keeps
  nor replaces one. A custom role that a project's member holds is in use too.

Roles in projects: `POST /_rustybin/users` takes `projects` (`[{project, roles}]`);
with some, the instance roles may be empty and the user sees those projects only.
Each user's answer lists their `projects` (`sso: true` for roles a single sign-on
group gives). See [Projects](projects.md).


```hurl
{{#include ../examples/concepts/users.hurl:guard_rails}}
```

## Resets, sessions, disabling

A forgotten password is a new invitation: `POST /_rustybin/users/{id}/invite`
answers a new link (a reset) that replaces the open one. The old password works
until the link is accepted; accepting it ends the user's sessions.

```hurl
{{#include ../examples/concepts/users.hurl:reset}}
```

`GET /_rustybin/users/{id}/sessions` lists a user's sessions, `DELETE` signs them out
everywhere:

```hurl
{{#include ../examples/concepts/users.hurl:sessions}}
```

Disabling a user (`"disabled": true`) ends their sessions at once and drops their
open invitation; their sign-in gets the same answer as a wrong password, so nobody
learns the account exists. Enabling them again does not bring an old token back.
Disabling or deleting a user also revokes every AI agent acting for them (their
sign-in and OAuth agents: [Agents act for a user](agent-mcp.md#agents-act-for-a-user));
enabling the user again leaves those agents revoked until someone restores them.

```hurl
{{#include ../examples/concepts/users.hurl:disable}}
```

## As code

[`rustybin.yaml`](configuration-as-code.md) carries `roles:` (the custom roles) and
`users:` (name, display name, email, roles, disabled; never a password or a hash: a
document with one does not load that section). Applying needs `users.manage`. A user
the document creates is invited and the apply answers the link to send them (a user
it creates disabled gets none until enabled; a file loaded at startup has nobody to
answer, so its new users wait for `POST /_rustybin/users/{id}/invite`). With `prune`,
users and custom roles the document does not name are deleted, but never the first
admin, the applying user, the last enabled admin, or a role somebody still holds.

```hurl
{{#include ../examples/concepts/users.hurl:document}}
```

A document pulled from [source control](source-control.md) is untrusted: anyone who
can push would otherwise make themselves admin. It changes display names and emails
only. A new user, a change of a user's roles or of `disabled`, a prune of users, and
a privileged role defined, changed or pruned are reported in `problems` and kept as
they are.

## In the console

**Management, Users and roles**: the users with their roles, status and last
sign-in; **Invite** with the link to copy (shown once); per user **Roles**,
**Disable** or **Enable**, **Reset** (or **New link** while invited) and delete; the
roles' permission matrix with the privileged roles marked; a form for custom roles.
The account menu (your name in the top bar) opens **My account**: your profile, your
permissions, your password and your sessions. The link of an invitation opens the
console's join page, which takes the token out of the address bar at once.
**Invite a portal reader** invites with the `portal_reader` role alone: the link opens
the [API portal](portal.md#portal-logins), and a console sign-in with that login is
sent there.

## Limits

| What | Limit |
|---|---|
| Users | 500, in `<state>/users.json` (mode 0600; in memory without a state directory) |
| Open invitations | 100, 72 hours each, one per user |
| Custom roles | 50, 1 to 40 characters a name, 200 a description |
| Roles per user | 16 |
| Display name, email | 100 and 254 characters |

## On every surface

| Surface | Where |
|---|---|
| API | `GET`, `POST /_rustybin/users`; `GET`, `PATCH`, `DELETE /_rustybin/users/{id}`; `POST /_rustybin/users/{id}/invite`; `GET`, `DELETE /_rustybin/users/{id}/sessions`; `GET`, `POST /_rustybin/roles`; `GET`, `PUT`, `DELETE /_rustybin/roles/{name}`; `GET`, `PATCH /_rustybin/me`; `PUT /_rustybin/me/password` (and the 0.3 `PUT /_rustybin/password`); `GET`, `DELETE /_rustybin/me/sessions`, `DELETE /_rustybin/me/sessions/{id}`; `POST /_rustybin/join` (open) |
| Console | **Management, Users and roles**; the account menu's **My account**; the join page an invitation link opens |
| CLI | `rustybin users list`, `invite`, `show`, `update`, `disable`, `enable`, `sessions`, `delete`; `rustybin roles list`, `show`, `save`, `delete`; `rustybin me` (`update`, `sessions`, `end-session`); `rustybin passwd` ([reference](cli.md#users)) |
| Management MCP | `list_users`, `invite_user` (confirmed; answers the link once), `update_user` (confirmed when it grants a privileged role), `delete_user` (confirmed), `list_roles`, `save_role` (confirmed when privileged), `delete_role`; `list_sessions` and `end_session` with `user`. No tool takes or returns a password or a hash ([agent page](agent-mcp.md#agents-users-and-sign-in-sessions)) |
| `rustybin.yaml` | `users:` and `roles:` ([instance as code](configuration-as-code.md)) |
