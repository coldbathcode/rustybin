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

## Build notes (2026-11-03)

- **The safety net came first.** Before anything changed, the 0.4 app was called with every
  credential (no token, the `inspector`, `harness`, `console` and `admin` scopes, the admin token;
  an open control plane with an admin token, in public mode, with self-service; shared mode with a
  key, with and without self-service) on every catalogue route and method, and the answers were
  read with every `require_admin` call site. The result is frozen in `src/rbac/access_table.rs`:
  `old_grant_allows` (0.4's `Grant::allows` with its path rules), `OLD_ADMIN_ONLY` (every route and
  method whose handler needed admin: the 73 call sites, the sign-in management routes, the visit
  operator routes), `OLD_OPEN_ADMIN_ONLY` (the harness mutations an open control plane guarded),
  `old_key_fence` (what the key layer let a key reach). `the_old_scopes_reach_what_they_reached`
  compares them, for 184 routes and methods and five callers, with what the declarations give now;
  `DELIBERATE` lists every difference with its reason (the test fails on an unlisted one and on a
  listed one that is no longer a difference). `the_route_check_enforces_the_declarations` calls the
  full app with the same callers on every route and checks that the check answers what the
  declarations say (a refusal names the declared permission; an allowed call is never refused by
  the route check).
- **The scopes keep their 0.4 reach (deviation from step 07).** Step 07 mapped `console` to the
  viewer role and `harness` to `harness.read` plus `harness.run`; that would have taken captured
  traffic, the AI assist's read tasks and the MCP inspector from every `console` token and cluster
  management from `harness` tokens. Now `console` is the viewer role plus `traffic.read`,
  `assist.use` and `mcp.inspect` (`rbac::CONSOLE_SCOPE_EXTRA`, its roles still say `viewer`) and
  `harness` is every `harness.*`. The built-in `viewer` role is as planned (no traffic, no assist,
  no inspector). Say if the scopes should shrink to the roles instead.
- **The deliberate differences** (all widenings, none narrows what a 0.4 credential could do):
  - the `console` scope reads what the viewer role reads and 0.4 kept for admin: `GET
    /_rustybin/settings`, `/diagnostics`, `/egress`, `/assist/usage`, `/keys/usage` (all
    `settings.read`), `/tests`, `/tests/{name}`, `/tests/runs`, `/tests/runs/{id}`,
    `/tests/export`, `/tests/schedules`, `/tests/schedules/{name}` (`tests.read`), `/git`,
    `/git/key`, `/git/history` (`git.read`), `/agents`, `/agents/events`, `/agents/{id}`
    (`agents.read`); and `POST /_rustybin/harness/preview` and `/validate` (`harness.read`: they
    compute and send nothing);
  - the `console`, `inspector` and `harness` scopes may `POST /_rustybin/logout`, and reach
    `POST`/`DELETE /_rustybin/mocks/visit`, `/visit/extend`, `/visit/token` (a visit's own routes,
    which the manage token decides and which answer `404` with control auth on, where
    self-service is off); the `inspector` and `harness` scopes read `GET /_rustybin/session`;
  - the `console` and `inspector` scopes may `DELETE /_rustybin/requests?session=...` (one
    session's captures, `traffic.read`, as an open control plane always allowed); everyone's
    captures need `traffic.clear` (a body check);
  - the `inspector` scope reads `/_rustybin/mocks/{name}/requests*` (`traffic.read`; it already
    read the same captures under `/_rustybin/requests`);
  - on the management server the same, by tool: the `console` scope lists the `settings.read`,
    `tests.read`, `git.read` and `agents.read` tools, the `harness` scope the `harness.read` ones;
    `export_config` is a `mocks.read` tool (the whole instance still needs `config.export`, as
    `is_admin` did), so a key and nobody on an open instance still see it.
  - Not who but how: a refusal is `reason: permission_required` with `permission` (was
    `insufficient_scope` with the caller's `scopes`, or an admin message); an API key refused by
    a handler in shared mode gets `403` (was `401`; the key layer already answered `403`); a
    method a route does not have still answers `405`.
- **The one check** is `control_auth::check_route`, a `route_layer` on the merged routers in
  `build_app` (so it sees `MatchedPath` and runs before the handler and its body extractors).
  `enforce` only authenticates now and leaves the caller's `Resolved` in `ControlAuthorized`; the
  key layer leaves an admin's there in shared mode. `caller_of` resolves what is not there: a
  shared-mode key is `Resolved::key` (the anonymous reads, inside the key layer's unchanged fence),
  nobody on an open control plane is every permission without an admin token outside public mode,
  else `rbac::need::ANONYMOUS` (`instance.read`, `mocks.read`, `traffic.read`, `harness.read`,
  `assist.use`, `mcp.inspect`, which is exactly what such a caller could reach in 0.4). Such a
  refusal is `401` with an admin token configured, `403` in public mode without one, as the admin
  guard answered. The catalogue lookup is `catalog::control_route(template)`;
  `control_route_for_path` matches a concrete path the way the router does (tests, the CLI
  reference).
- **Self-service and keys keep their per-mock rules.** Every `/_rustybin/mocks*` route and
  `/_rustybin/portal/items/{id}` say `.self_service()`: on a self-service instance a caller without
  the permission reaches the handler, where `selfservice::Caller` decides (its `admin` is now
  "holds `mocks.write`", its refusals `Caller::need(permission)`). The visit operator routes and
  tools need `visits.operate`.
- **Declarations worth knowing**: applying a mocks document and reloading the files are
  `config.apply` (not `mocks.write`: the editor builds mocks one by one, as the plan's roles say);
  testing the AI key and loading the models `settings.write`, the usage `settings.read`;
  checkpoints' lists `mocks.read`; a scenario run's state `instance.read` (as open as in 0.4;
  runs are owned by the session key); the portal's control view and insights, the registry view
  and `validate` `instance.read`, registry browsing `mcp.inspect`; `DELETE` of a check run and the
  relay's saved credentials and outward calls `mcp.connections`; a mock's state `mocks.read` to
  read, `mocks.data` to reset (its request log too); the data-plane `/health` toggles and the
  global `/flaky` reset `settings.write` (`admin::require`). The permission summaries say so
  (`instance.read` no longer names diagnostics, which are `settings.read`).
- **Body checks** (`rbac::need::BODY_CHECKS`, documented in the control-plane security page):
  `traffic.clear` for everyone's captures; `settings.security`, `portal.settings` and
  `secrets.write` per settings key (`settings::extra_permissions`, also in `GET
  /_rustybin/settings` as each setting's `needs`, which the console reads); a document's
  `settings:`/`instance:` (`settings.write` and the keys'), `secrets:` and `agents:`
  (`configdoc::check_sections`, the REST route and `apply_config` alike); `mcp.connections` in
  the relay and checks; `git.read` for the repository's checkpoints; `assist.jobs` for an assist
  task that writes (no task writes today: AI-built mocks go through `/_rustybin/mocks/ai/*`, which
  need `assist.jobs`).
- **Tools**: `ToolDef.permission` is a `ToolNeed` (`Perm(p)`, `Owner(p)` for the mock changes a
  visitor or key may make on its own mocks, `Visitor` for `get_visit` and `extend_visit`).
  `every_tool_declares_a_permission` checks the kinds and the Permission column of
  `docs/src/agent-mcp.md`. Ceilings read permissions: `read-only` is a read-only tool that does not
  need `traffic.read`, `manage` a tool whose permission is in `MANAGE_CEILING`;
  `ceilings_keep_their_tool_counts` compares every tool under every ceiling with the 0.4 rule.
  `get_instance` answers `caller.roles` and `caller.permissions` (no more `rights`), an agent's
  tool list each tool's `permission`.
- **API**: the OpenAPI document gives every control operation `x-rustybin-permission` (a permission,
  `open` or `signed in`) and a `403` for a permission; `every_control_route_declares_a_permission`
  (in `src/rbac/access_table.rs`, beside the table it guards) checks the catalogue and the
  document, and that the open routes are exactly the ones control auth leaves alone.
- **Unused permissions**: `every_permission_is_used` counts routes, tools and the listed body
  checks; `portal.read` (step 13) and `audit.read` (step 14) are `RESERVED` with their step.
- **CLI (deviation)**: exit code 3 already meant "the network or the credentials"; a missing
  permission joins it as `Fail::Permission`, the message starting `permission_required: <p>` (4 is
  free if the owner wants its own code). The generated reference has a **Needs** column from the
  routes each command calls.
- **Console**: `ui/lib/perms.js` loads `GET /_rustybin/session`'s permissions (reloaded with the
  status; a view is mounted again when they change) and `perms.guard(el, p)` disables a control
  with a title naming the permission: mocks (through `readOnlyReason`), settings forms (each
  setting's `needs`), saved tests and schedules, checkpoints, source control, the export's apply,
  agents, portal publishing, the harness's run, stop, save, live changes and cluster, the
  password and sessions; the live feed does not connect without `traffic.read`. Unknown (an open
  control plane) leaves every control as it was. `conformance/ui/console_e2e.py` signs in to the
  jwt instance with `rb_roles` tokens (a viewer: no enabled write control; an editor: mocks and
  tests enabled, settings read-only), so no `users.json` is needed before step 09's user API.
- **For step 09**: users are created in tests through `state.users` (and `Users::save_role`);
  `/_rustybin/users*` needs `users.read` / `users.manage` (declared on the sessions and password
  routes already). The tool descriptions still end in "Admin." where 0.4 said so; the permission
  column is the truth.
