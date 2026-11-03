# Projects

Several teams can share one instance without stepping on each other: the payments
team edits its mocks and tests, and sees the rest read-only or not at all. A
**project** groups mocks, saved tests and checkpoints (a mock's [portal](portal.md)
item follows its mock), and a user's roles **in a project** apply to its resources on
top of their [instance roles](users.md).

- **The union rule.** A permission on a resource of a project is the union of the
  user's instance roles and their roles in that project. An `editor` of `payments`
  with the instance role `viewer` edits payments' mocks and reads everything else;
  with no instance role at all, they see payments only.
- **Without a project** a resource is the instance's, as everything was before
  projects: only instance roles reach it. A user with instance roles only sees and
  does exactly what they did before.
- **Instance-wide things** (settings, egress, users and roles, source control,
  captured traffic of the whole instance, the harness, gateway scenarios, schedules)
  need instance roles. A user with a role in a project also holds `instance.read`,
  so the console's status and catalogue load.
- **Another project's resource** a user may not read answers `404`, as other
  visitors' mocks do, and every list leaves it out. Nor do project names leak: naming
  a project that does not exist answers like naming one the caller may not use
  (`403 permission_required`; only a caller who holds the permission on the whole
  instance hears `400 unknown_project`), and a checkpoint's restore says a mock is
  "now in another project" without naming it. `GET /_rustybin/projects` lists a
  project's `resources` (mock, test and checkpoint names) only to who reads them there
  (`mocks.read`, `tests.read`): `users.read` sees the project and its members, not its
  contents. The mocks' load errors (which name any mock) go to who holds `mocks.write`
  on the whole instance only.
- **Moving** a mock or a saved test to another project needs the write permission
  (`mocks.write`, `tests.write`) in both.
- **The guard rails** stay instance-wide: a project's `admin` role never counts as
  an admin of the instance, so the last enabled admin stays one.

At most 100 projects. A project's name follows the mock name rules (lower case
letters, digits and `-`) and is never `instance`, which stands for the instance's
resources in `?project=instance` and `move_to=instance`.

## Create a project and add people

`PUT /_rustybin/projects/{name}` (`users.manage`) creates the project or changes it:

```hurl
{{#include ../examples/concepts/projects.hurl:create}}
```

`members` (`[{user, roles}]`, a user by id or sign-in name) replaces the member
list. The rules of [users and roles](users.md) hold in a project too: nobody changes
their own roles, and a caller gives or takes only roles whose permissions it holds
there. An invitation may give roles in projects (`projects`); with some, the instance
roles may be left empty:

```hurl
{{#include ../examples/concepts/projects.hurl:invite}}
```

The session says what the user holds where:

```hurl
{{#include ../examples/concepts/projects.hurl:session}}
```

## Resources in a project

A mock, a saved test or a checkpoint names its project with `project` when it is
created (`POST /_rustybin/mocks`, `POST /_rustybin/tests`, `PUT
/_rustybin/tests/{name}`, `POST /_rustybin/checkpoints`). A caller without the
instance-wide permission who works in exactly one project and names none creates in
that project:

```hurl
{{#include ../examples/concepts/projects.hurl:mock}}
```

Every list takes `?project=` (`instance`: the instance's resources only) and leaves
out what the caller may not read:

```hurl
{{#include ../examples/concepts/projects.hurl:list}}
```

`PATCH /_rustybin/mocks/{name}` with `project` (a name, `null` or `instance`) moves a
mock; `PUT /_rustybin/tests/{name}` with another `project` moves a test. A **project's
checkpoint** (`POST /_rustybin/checkpoints` with `project`) keeps that project's mocks
only, and its restore changes only those, never the instance's health toggle. A run
of the saved tests (`POST /_rustybin/tests/run`) runs the tests the caller may run
(`project` narrows it), and the history shows a member the runs of their project's
tests. Checkpoints in the [configuration repository](source-control.md) and the test
schedules stay the instance's.

Instance-wide things refuse a member of a project:

```hurl
{{#include ../examples/concepts/projects.hurl:instance}}
```

## Delete a project

A project that holds mocks, saved tests or checkpoints is deleted only with
`move_to` (another project, or `instance`), which needs the write permission of each
kind of resource in both; its members' roles there go with it:

```hurl
{{#include ../examples/concepts/projects.hurl:delete}}
```

## Single sign-on

An entry of `sso.role_map` may name a project: `{group, project, roles}` (kept as
`group=project:role role`), so a group gives roles in a project at every sign-in. A
group that gives roles in a project only takes no default roles. See
[single sign-on](sso.md).

## AI agents

An [agent acting for a user](agent-mcp.md#agents-act-for-a-user) follows the user's
project roles at every call, within its own ceiling: it lists the tools the user
holds somewhere, and each call checks the resource's project. The OAuth consent page
shows what the agent will hold in each of the user's projects, and a member of a
project only may grant a scope that gives them something there. A live traffic stream
(`GET /_rustybin/mocks/{name}/requests/stream`) checks its caller again at every event
and every few seconds: removed from the project, disabled or signed out, the caller
gets nothing more and the stream ends. The AI spec job (`POST
/_rustybin/mocks/ai/spec`) tells the model which mock names are taken from the names
the caller may read only.

## On every surface

| Surface | Where |
|---|---|
| API | `GET /_rustybin/projects`; `GET`, `PUT`, `DELETE /_rustybin/projects/{name}` (`?move_to=`); `project` on `POST /_rustybin/mocks`, `PATCH /_rustybin/mocks/{name}`, `POST /_rustybin/tests`, `PUT /_rustybin/tests/{name}`, `POST /_rustybin/tests/run`, `POST /_rustybin/checkpoints`; `?project=` on `GET /_rustybin/mocks`, `/tests`, `/tests/export`, `/checkpoints`; `projects` on `POST /_rustybin/users` and in the users' answers; `projects` in `GET /_rustybin/session` |
| Console | the **Project** picker in the top bar (filters the mocks, saved tests and checkpoints; new ones go to it); a mock's **Settings**, **Project** (move it); **Management, Users and roles**: the **Projects** card (create, members, delete) and each user's project roles |
| CLI | `rustybin projects list`, `show`, `save`, `delete --move-to`; `--project` on `mocks list`, `mocks create`, `mocks move`, `tests list`, `tests save`, `tests run`, `checkpoint list`, `checkpoint save`; `users invite --project P=role` ([reference](cli.md#projects)) |
| Management MCP | `list_projects`, `save_project` (confirmed when it gives a privileged role), `delete_project` (confirmed, `move_to`); `project` on `list_mocks`, `create_mock`, `update_mock` (a move), `list_tests`, `save_test`, `run_tests`, `list_checkpoints`, `save_checkpoint`; `projects` on `invite_user` ([agent page](agent-mcp.md#agents-users-and-sign-in-sessions)) |
| `rustybin.yaml` | `projects:` with members by user name, `project:` on mocks and tests ([instance as code](configuration-as-code.md#projects)) |
