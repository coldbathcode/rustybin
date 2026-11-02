# Step 02: One validated plan for preview and apply

## Goal

A dry run validates everything the apply validates and refuses what the apply would refuse, without
changing anything. What cannot be made atomic is said so in the answer and the docs.

## Fixes

| Where | Problem | Fix |
|---|---|---|
| `apply_settings` | returns before validation in a dry run | `Settings::check(values, secrets)`: the validation half of `update` (kinds, checks, locks, sizes, the public password guard), used by the dry run and by `update` itself |
| `apply_secrets` | references resolved only on apply | resolve in the dry run too (never return values): unset variables are problems in both |
| `apply_mocks` | previews never compile | compile every definition in the dry run (off the runtime, as the apply does) and report compile errors and limit problems |
| registry, portal, harness, agents sections | dry runs skip some checks | each section's dry run runs the same validation as its apply |
| the answer | `ok: true` with problems the apply would hit | `ok` is false when the preview found a problem the apply would also report |

## Tests

A numeric `console.title` fails the dry run with the apply's message and changes nothing; a mock
whose spec does not compile fails the dry run; a locked setting is reported by the dry run; a
preview then an apply of the same document report the same problems.

## Build notes (2026-11-02)

- `Settings::check(values, secrets)` runs `Settings::validated`, the validation half
  `update` now calls first (kinds, normalisers, checks, locks, sizes, the public
  password guard), plus the key and secret counts; nothing is written.
- Dry runs: settings and secrets call `check`; mocks compile every definition (the
  configuration apply and `POST /_rustybin/mocks/apply?dry_run`) and check the
  instance's mock capacity; the portal runs `clean_meta` and its capacity; the harness
  its capacity. Secret references were already resolved before the dry run returned;
  registry and agents already validated before it.
- `ok` was already `problems.is_empty()`, so a preview problem makes it false.
- What is not atomic (sections in order, a failed entry does not stop the others) is
  stated in `docs/src/configuration-as-code.md`.
