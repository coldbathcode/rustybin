# Step 07: Users, roles and permissions: the model

Access control, chapter 1 of 7 (steps 07 to 13). This step builds the model and the store; step 08
makes every route enforce it, step 09 puts users on every surface.

## Goal

Several people sign in to one instance, each with a role that says what they may do. A role is a
set of named permissions; the four built-in roles cover the usual team, custom roles cover the
rest. The single console user of 0.3 becomes the first admin, with nothing to do on upgrade.

## What exists (0.3.1)

- `login.rs`: one user (`console.user`, `console.password_hash`, argon2id), sign-in tokens with
  `scope: admin`, `rb_gen` (the generation that "sign out everywhere" rotates), `jti`, `Tokens` for
  revocations, sessions and failed attempts.
- `control_auth::Grant`: four booleans (`admin`, `console`, `inspector`, `harness`) from the token's
  `scope`, `Grant::allows(method, path)` with hand-written path rules.
- `admin::require_admin`: 73 call sites in 22 files, a yes/no check.
- The management MCP: `Right` per tool (`read`, `traffic`, `manage`, `admin`, `visitor`, `operator`,
  `harness`); agents' ceilings on top (`read_only`, `traffic`, `manage`, `full`).

## Model

**Permissions** (`src/rbac/permission.rs`): one enum, `Permission`, each with a stable dotted name,
a group and a one-line summary (the console and the docs render the table from it). The first set:

| Group | Permissions |
|---|---|
| Instance | `instance.read` (status, catalogue, version, diagnostics), `settings.read`, `settings.write`, `settings.security` (the `egress.*`, `assist.provider`, `assist.base_url`, `proxy.*`, `agents.*`, `console.*`, `sso.*` groups), `secrets.write` |
| Mocks | `mocks.read`, `mocks.write` (create, edit, deploy, roll back, on/off), `mocks.delete`, `mocks.data` (stateful data, reset) |
| Traffic | `traffic.read` (captures, bins, mock traffic), `traffic.clear` |
| Testing | `tests.read`, `tests.write`, `tests.run`, `scenarios.run`, `checkpoints.write`, `checkpoints.restore` |
| Load | `harness.read`, `harness.run` (start, change, stop runs), `harness.cluster` |
| AI | `assist.use`, `assist.jobs` (AI-built mocks) |
| MCP | `mcp.inspect` (inspector and relay), `mcp.connections` (saved connections and their credentials), `registry.write` |
| Portal | `portal.publish`, `portal.settings` |
| As code | `config.export`, `config.apply`, `git.read`, `git.write` |
| People | `agents.read`, `agents.manage`, `users.read`, `users.manage`, `audit.read` |
| Self-service | `visits.operate` |

Every permission is checked somewhere (a test fails on an unused one); a new feature adds its
permissions here, never a yes/no admin check.

**Roles** (`src/rbac/role.rs`): a name, a description, a permission set.

| Built-in role | What it is for | Permissions |
|---|---|---|
| `viewer` | Look around: the console read-only (today's `console` scope) | every `*.read` except `traffic.read`, `audit.read`, `users.read` |
| `tester` | Run what others built | `viewer` plus `traffic.read`, `tests.run`, `scenarios.run`, `harness.read`, `harness.run`, `assist.use`, `mcp.inspect` |
| `editor` | Build mocks and tests | `tester` plus `mocks.*`, `tests.write`, `checkpoints.*`, `assist.jobs`, `portal.publish`, `registry.write`, `config.export`, `git.read`, `traffic.clear` |
| `admin` | Everything | every permission |

Built-in roles cannot be changed or deleted. Custom roles (at most 50) pick any permissions; a role
that grants `users.manage` or `settings.security` is marked privileged in every view.

**The old scopes** keep their meaning, as roles: `admin` is `admin`, `console` is `viewer`,
`inspector` is `traffic.read` alone, `harness` is `harness.read` and `harness.run`. A control JWT may
also carry `rb_roles` (role names) and `rb_perms` (permission names); unknown names are ignored and
logged once, never an error, so a platform can roll out a newer token format first.

**Users** (`src/rbac/users.rs`, `AppState.users`, `<state>/users.json`, 0600, at most 500): an id
(`usr_` plus 16 random hex), a name (the sign-in name, `login::valid_user`), a display name, an
email (optional, never logged), roles, `disabled`, `source` (`password`, `sso`, `document`), the
argon2id hash (never returned, never exported), `created_at`, `last_sign_in`, a per-user generation
(signing one user out everywhere rotates theirs only). Writes go through `off_runtime` with the
latest change winning, as the visits store does since 0.3.1. Without a state directory, users live
in memory and a warning says so (diagnostics has a check).

**Resolution**: a request's permissions are resolved at every request from the token's user id (so
a role change or a disable applies at once, not at the token's expiry), cached for one second per
user. A sign-in token carries `sub` (the user id), `rb_gen` (the instance's) and `rb_ugen` (the
user's); the admin token is every permission (break glass, never a user); an open control plane is
every permission, as today.

## Migration

- On first start with 0.4, the 0.3 user (`console.user` and `console.password_hash`, or the
  environment's `RUSTYBIN_CONSOLE_USER` and `RUSTYBIN_CONSOLE_PASSWORD*`) becomes user one with the
  `admin` role. The settings stay readable for a downgrade; a later change goes to the user store.
- Sign-in tokens issued by 0.3 (`scope: admin`, no `sub` user id) stay valid as that first user
  until they expire.
- The environment keeps seeding the first admin on every start (a deployment that sets the
  password in the environment keeps that working); the default password rule (`public mode refuses
  to start`) applies to that user.

## Surfaces

Nothing user-facing in this step beyond `GET /_rustybin/session` answering the caller's user, roles
and permissions (the console needs it from step 08 on). The store, the model and the migration are
the step.

## Tests

The built-in roles' sets (a table test, so a change is deliberate); custom role validation (unknown
permission, name rules, the cap); the old scopes map to the roles above; `rb_roles` and `rb_perms`
claims, unknown names ignored; the migration from a 0.3 state directory and from the environment;
a 0.3 token still signs in as user one; a role change and a disable apply on the next request; the
store's cap, its file mode and a write that fails changes nothing.
