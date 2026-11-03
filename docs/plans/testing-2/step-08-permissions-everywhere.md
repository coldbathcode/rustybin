# Step 08: Every route and tool checks a permission

Access control, chapter 2 of 8.

## Goal

Every control-plane route and every management tool names the permission it needs, the check is
made in one place, and a route or tool without one cannot be merged. The yes/no admin check is
gone.

## Model

- **The catalogue declares it.** `Endpoint::permission(Permission)` on every `/_rustybin/*` route
  (per method where they differ: `GET` mocks is `mocks.read`, `POST` is `mocks.write`); the open
  routes (`/_rustybin/ready`, `login`, the OAuth sign-in endpoints) say `.open()`. A parity test,
  `every_control_route_declares_a_permission`, fails on a route without either, as the catalogue
  tests do for examples today.
- **One check.** `control_auth::enforce` resolves the caller (step 07) and checks the route's
  permission from the matched route template before the handler runs; `403` with `{"reason":
  "permission_required", "permission": "mocks.write"}` names what is missing (never what the caller
  holds). `Grant::allows` and its path rules go; `admin::require_admin`'s 73 call sites become the
  catalogue declaration, plus `rbac::require(&caller, Permission)` inside handlers whose need
  depends on the body (a settings `PATCH` touching a `settings.security` group, a config apply with
  `secrets:`).
- **Tools declare it too.** `ToolDef.permission` replaces `Right` (`visitor` stays a rule of its
  own: a visit's manage token); `tools/list` lists what the caller's permissions allow, then the
  agent's limits (step 12). `every_tool_declares_a_permission` joins
  `every_control_route_has_a_management_tool`.
- **Per-mock rules stay.** A visitor and an API key still manage only their own mocks; a user's
  permission applies to the instance's mocks (projects narrow it in step 11).
- **The CLI and the console ask, never guess.** `GET /_rustybin/session` answers the permissions;
  the console hides or disables what the caller cannot do (a button says which permission it
  needs), the CLI prints `permission_required: mocks.write` and exits 3 (a new exit code).

## Compatibility

- `RUSTYBIN_CONTROL_AUTH` keeps its four modes; `token` mode is the admin token only (every
  permission), `jwt` mode reads `scope`, `rb_roles` and `rb_perms`, `password` mode reads the user.
- A route's need must not shrink by accident: the step starts with a table test that records, for
  every route and method, which of today's four scopes may call it, and asserts the new
  declarations give the same answer for the built-in roles mapped from those scopes. Every
  difference is listed in the build notes as a deliberate change.

## Surfaces

The behaviour of every surface changes underneath; the new visible parts are the `403` body, the
console's disabled controls with their reason, the CLI's exit code 3, and the permission column in
`docs/src/agent-mcp.md`'s tool tables and in the generated CLI reference.

## Tests

The parity tests above; the scope table test; a viewer, a tester and an editor against a sample of
routes per group (allowed and refused, with the permission named); the body-dependent checks; a
disabled user refused at the next request; the console conformance for a viewer (no write buttons
enabled) and an editor (settings read-only).
