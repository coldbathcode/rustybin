# Step 15: Docs and release 0.5.0

## Goal

The book covers every new workflow and the access control (pages updated, "On every surface"
tables, tested Hurl examples), the changelog has a `0.5.0` entry, the roadmap moves the milestone
to "Recently built", and the release follows the 0.2.0 recipe (tag, signed image, GitHub release
with the binaries, digest commit) once the owner says go.

## Pages

- The testing workflows: saved tests (history, schedules), gateway scenarios (the four new ones),
  checkpoints in git, the compare view.
- Access control: a new chapter "Teams and access control" with pages for users and invitations,
  roles and the permission table (generated from `Permission`, like the CLI reference, with
  `RUSTYBIN_UPDATE_RBAC_DOCS=1`), single sign-on (with the demo identity provider as the worked
  example), projects, open and closed APIs on the portal (in `docs/src/portal.md` too), agents acting for a user, and the audit log.
- `docs/src/concepts/control-plane-security.md` rewritten around users and roles; the
  configuration tables with every `sso.*`, `audit.*` and `agents.*` setting; the coding agents page
  says whose permissions an agent gets.
- **Upgrading from 0.3**: the console user becomes the first admin, 0.3 tokens keep working until
  they expire, the old scopes map to roles, what a `403` now names, the CLI's exit code 3, and the portal: every
  published item stays open, `portal.auth=password` now asks for a portal sign-in (a user with
  `portal.read`) instead of storing a console token, and closing a mock
  turns on its mocked security.

## Release

The behaviour changes first in the changelog (the `403` body, exit code 3, per-request permission
resolution, the scope-to-role mapping and every deliberate difference the step 08 table test
listed); the conformance suites (ui with several users, oauth with SSO) green before the tag.
