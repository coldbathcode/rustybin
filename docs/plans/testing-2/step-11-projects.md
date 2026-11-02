# Step 11: Projects: roles on part of an instance

Access control, chapter 5 of 7.

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
