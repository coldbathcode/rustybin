# Step 07: Users, roles and permissions: the model

Access control, chapter 1 of 8 (steps 07 to 14). This step builds the model and the store; step 08
makes every route enforce it, step 09 puts users on every surface.

## Goal

Several people sign in to one instance, each with a role that says what they may do. A role is a
set of named permissions; the five built-in roles cover the usual team, custom roles cover the
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
| Portal | `portal.read` (closed items on the portal, step 13), `portal.publish`, `portal.settings` |
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
| `portal_reader` | A tester who reads the API portal's closed items (step 13) and opens nothing in the console | `portal.read` |

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

## Build notes (2026-11-03)

- **Where it is.** `src/rbac/`: `permission.rs` (38 permissions, the table above in order, a bit set
  `PermSet`), `role.rs` (the five built-in roles, custom role validation: lower case name of 1 to 40
  characters starting with a letter, not a built-in name, at least one known permission, no repeats,
  a description of at most 200 characters; privileged when it grants `users.manage` or
  `settings.security`), `users.rs` (the store), `mod.rs` (`Resolved`, the scope mapping, the JWT
  claims, `from_sign_in`, `grant_of`, `migrate`, the diagnostics check `users`), `tests.rs`.
- **The store lives in `Config.users` too** (as `Tokens` does in `Config.console_tokens`): every
  credential check takes `&Config`. `AppState.users` is the same `Arc`.
- **Writes**: a change is made on a copy of the document, written (off the runtime, one writer at a
  time, so the latest change wins) and published only when the write succeeded; a failed write
  changes nothing and sets the diagnostics' `users` check to `fail`. A `users.json` that cannot be
  read is never written over (the users stay in memory, `fail`). Password hashes are a map beside
  the users in the file, so `User` has no hash field and cannot leak one through an answer or an
  export.
- **Deviation: user one's credentials stay in the sign-in settings.** User one's name and password
  are still `console.user`, `console.password_hash` and the environment (unchanged precedence, the
  settings lock, `PUT /_rustybin/password`, the `PATCH /_rustybin/settings` rules, the admin token
  reset), and its record in the store follows the name. So a downgrade signs in, the environment
  seeds user one on every start and the default password rule applies to it, with every 0.4 test
  unchanged. "A later change goes to the user store" is left to step 09, whose `/me/password`
  replaces `PUT /_rustybin/password`; other users' hashes are in the store already (`Users::add`).
- **Migration** runs in `AppState::with_clock` (`rbac::migrate`) in password mode only (not shared
  mode, not the other modes): the effective sign-in name becomes user one with `admin`, or user
  one's name follows it. A rename through the sign-in settings follows into the store.
- **Tokens**: `sub` the user id, `rb_user` the sign-in name (the console's "Signed in as" and the
  agents' `Credential::Console`, which keeps an agent's identity across the upgrade), `rb_gen`,
  `rb_ugen`, and `scope` from the user's grant (informational; what a 0.4 binary would honour after a
  downgrade). A token without `rb_ugen` counts as generation 1. A 0.3 or 0.4 token (`sub: "user:<name>"`)
  is user one, and follows user one's roles, disable and generation.
- **Disable ends sessions**: `set_disabled(true)` also moves the user's generation on, so enabling the
  user again does not revive an old token (step 09's guard rail, done in the model).
- **Resolution cache**: a second per user, and dropped at every change of the store, so a role change
  applies to the next request even inside the second; `Users::cache_misses` lets the test see the
  cache without sleeping (a `ManualClock`).
- **Grant from permissions**: `Grant::from_scope_claim` and every token's grant now come from
  `rbac::grant_of(permissions)`; `the_old_scopes_keep_their_grants` compares it with the 0.4
  implementation for every combination of the four scopes (and unknown ones, absent, null, junk).
  `admin` is every permission, `console` the viewer's set, `inspector` `console` or `traffic.read`,
  `harness` `admin` or `harness.read` plus `harness.run`. So a `tester` or an `editor` user reads the
  control plane and mutates nothing until step 08, and a `portal_reader` is refused everywhere
  (`/_rustybin/session` included).
- **`rb_roles` / `rb_perms`**: a string or an array each; without a `scope` but with either, the token
  is what they say (no `console` default). Unknown names are logged once (at most 256 names
  remembered).
- **The session answer** keeps `user` (the name, which the CLI reads) and adds `user_id`,
  `display_name`, `roles` and `permissions`; the admin token answers the `admin` role and every
  permission. The sign-in answer and the sessions list add `user_id`. Open mode still answers `404`.
- **For step 08**: `control_auth::credentials_resolved` and `verify_jwt_resolved` answer a `Resolved`
  (`has(Permission)`), the place to check a route's permission; `ControlAuthorized` still carries the
  `Grant`. The session route should be reachable by any authenticated caller (an `inspector` token or
  a `portal_reader` cannot read it today). An API key in shared mode answers no permissions yet. The
  "unused permission" rule waits for the routes and tools to declare them; the table test lists all
  38.

