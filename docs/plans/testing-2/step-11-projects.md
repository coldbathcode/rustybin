# Step 11: Projects: roles on part of an instance

Access control, chapter 5 of 8.

## Goal

Several teams share one instance without stepping on each other: the payments team edits its
mocks and tests, sees the rest read-only or not at all.

## Model

- **A project** (at most 100): a name (the mock name rules), a description, its members. A mock, a
  saved test, a gateway scenario's saved setup, a checkpoint and a portal item may belong to one
  project (`project:`); without one they belong to the instance, as everything does today.
- **Bindings.** A user's roles are instance-wide (step 07) or per project: `{user, project, roles}`.
  A permission on a resource in a project is the union of the user's instance roles and their roles
  in that project. So an `editor` of `payments` with the instance role `viewer` edits payments'
  mocks and reads everything else; with no instance role, they see payments only.
- **Listing and reading.** Every list (mocks, tests, traffic of a project's mocks, checkpoints,
  portal items) is filtered by what the caller may read; another project's resource the caller may
  not read answers `404`, as other visitors' mocks do since 0.2. Instance-wide things (settings,
  egress, users, git) need instance roles.
- **Moving** a resource between projects needs the write permission in both.
- **SSO**: `sso.role_map` entries may name a project (`{group, project, roles}`).

## Surfaces

API (`/_rustybin/projects*`, a `project` field on the resources above, `?project=` filters),
console (a project picker in the top bar that filters every view, the project on each mock's
settings, Management, Users and roles shows bindings), management MCP (`list_projects`,
`save_project`, `delete_project` confirmed, `project` on the tools that create and list), CLI
(`rustybin projects ...`, `--project` on the commands that list and create), `rustybin.yaml`
(`projects:` with bindings by user name, `project:` on mocks and tests).

## Tests

The union rule with a table test; filtering on every list and the `404` on reads; a move between
projects; a project deleted only when empty (or with `--move-to`); the document round trip; the
console conformance with two users in two projects.

## Build notes (2026-11-03)

- **Where it is.** The model in `src/rbac/projects.rs` (a `#[path]` child of `users.rs`: projects live in
  `users.json` beside the users and roles, so a binding, its role and its user change under one writer), the
  service and routes in `src/rbac/project_api.rs`, the caller in `src/rbac/mod.rs` (`Resolved.projects`, `has_in`,
  `has_somewhere`, `has_projects`, `projects_with`, `capped`), the route check in `control_auth::check_route` (a
  `.project_scoped()` route lets in `has_somewhere` and leaves `RouteNeed` for the handler), the per-resource checks in
  `mocks::selfservice::Caller` (`can_read`, `may`, `can_manage`, `route_need`) and `mocks::service::Svc` (`readable`,
  `manageable`, `move_to`, `list_in`), `replay` (`visible`, `admit_for`, `delete_for`, `select_for`, `run_visible`; the
  kept runs carry their `projects` and `instance`), `checkpoints` (`list_for`, `save_for`, `delete_for`, a project's
  checkpoint), the store field `Entry.project` (saved with the mock), `MockDoc.project`, `SavedTest.project`, the
  tools in `src/mcp/manage/projects.rs`, the document in `src/configdoc/projects.rs`, the console in
  `ui/lib/projects.js`, `ui/lib/perms.js`, `ui/app.js` and `ui/views/manage/users.js`. Tests:
  `src/rbac/project_tests.rs` (12), `role_map_objects_name_a_project` in the SSO tests, `run_projects` in
  `conformance/ui/console_e2e.py`, `docs/examples/concepts/projects.hurl`.
- **The check takes the resolved caller only.** A project-scoped route or tool lets in a caller who holds the
  permission in some project; the service checks the permission on each resource in its project from the
  `Resolved` it is given. Nothing is agent-specific: step 12's agent caller (the user resolved at every call, capped
  by an OAuth token's scope) carries the user's project grants, `control_oauth::resolve_token` caps them by the
  token's scope too, and `agents::owner_allows` counts a permission held in a project for `Project` and `Owner`
  tools (`an_agent_acting_for_a_member_follows_their_bindings`). `Resolved::capped` caps the instance and every
  project at once for later limits.
- **Decision: a binding brings `instance.read`.** A user with roles in a project only must load the console (status,
  catalogue, the portal's control view); `instance.read` is what an anonymous caller of an open instance with an
  admin token reads anyway.
- **Decision: who manages projects.** Creating, describing, binding and deleting need `users.manage` (a binding is a
  grant); listing is any signed-in caller (`.authenticated`), who sees the projects they are in, or every one with
  `users.read` or the instance-wide `mocks.read`; members are shown with `users.read`. No new permission.
- **Decision: what belongs to a project.** Mocks, saved tests and checkpoints carry `project`; a mock's portal item
  follows its mock (the portal routes check the mock's project); other portal items, schedules, gateway scenario runs
  (there is no saved setup: runs are per session and last an hour), the harness, the inspector's captures and
  checkpoints in the repository stay the instance's. A member reads their mocks' traffic through
  `/_rustybin/mocks/{name}/requests*`. Departure from the plan: "a gateway scenario's saved setup" does not exist, so
  scenarios are not project resources.
- **Decision: the 0.4 rule for callers without bindings.** An instance mock without a project stays readable by any
  caller that reached the handler and holds no binding (a self-service visitor, as in 0.4); a caller with bindings
  reads the instance's resources through its instance roles only. So a caller with instance roles only meets exactly
  the checks of before (`instance_roles_reach_every_project`, and the access table is unchanged).
- **Decision: creating without naming a project.** A caller without the instance-wide permission who holds it in
  exactly one project creates there (the CLI and the tools need not name it); with several, `403` naming the
  permission. `instance` is reserved (never a project name) and stands for the instance's resources in `?project=`,
  `move_to` and the console's picker.
- **Decision: names stay instance-wide.** A mock, test or checkpoint name another project holds is a `409` on
  creation (the URL `/mock/<name>` is the instance's), which says the name is taken without saying whose.
- **Decision: a project's checkpoint** keeps that project's mocks; its restore creates, replaces and deletes only
  those (a mock now in another project is a problem, left as it is) and never touches the health toggle or the
  flaky counters. An instance checkpoint keeps each mock's project and restores it. A checkpoint from the repository
  never moves a mock between projects.
- **Decision: runs.** A run by a member without the instance-wide `tests.run` runs the tests it may run (of
  `project` when given); a name it may not see is `404`; a kept run records the projects of its tests, and the
  history shows a member the runs whose tests are all in projects they read. Clearing every run needs the
  instance-wide `tests.write`.
- **SSO.** `sso.role_map` keeps strings: a role written `project:role` is a role in that project (role and project
  names never hold `:`), and `{group, project, roles}` normalises to it. The roles a group gives in projects are kept
  on `SsoLink.mapped_projects` and joined with the member entries at resolution; a group giving project roles only
  takes no default roles, and such a user signs in (`no_role` only when nothing at all is mapped).
- **Users.** An invitation takes `projects` (`[{project, roles}]`), and with some the instance roles may be empty
  (also a document user named in `projects:` members); a user's answer lists `projects` (bindings, `sso: true` for a
  group's); managing a user needs their project roles' permissions in those projects; a deleted user leaves every
  project; a custom role a member holds is in use. The guard rail counts instance roles only.
- **Documents.** `projects:` is exported only when the instance has projects (so an instance without them exports as
  before and the snapshot holds); applied in two steps (the projects before the mocks, the members after the users),
  pruned only when a project holds nothing; `REPOSITORY_REFUSED_SECTIONS` lists the members, the prune and a move by
  `project:`.
- **Who can call what.** Two new routes in `NEW_SINCE_0_4`; no `DELIBERATE` row (a caller without bindings reaches
  exactly what it did). New: the 36 project-scoped routes and 22 project tools let in members of a project, and the
  management tools `list_projects` (`instance.read`), `save_project` and `delete_project` (`users.manage`; 136 tools
  for an admin, 117 without the harness ones, 70 under the `manage` ceiling).
- **For step 14**: the audit log should take the `projects` lines (`create_project`, `change_project`,
  `delete_project`, by and project) and a resource's moves; a project-scoped audit view (a project's admin reading
  its own trail) would fit the same `has_in` check.
