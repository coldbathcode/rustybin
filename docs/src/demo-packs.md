# Customer demo packs

A demo pack is everything one customer's demo needs, in one file: the mocks of their
APIs, the portal items that show them, the saved tests, the [storyboard](storyboards.md)
and a console title. You apply it before the meeting and tear it down after it, and the
instance is what it was before.

A pack is a [`rustybin.yaml`](configuration-as-code.md) document with a `pack:` header.
Applying it creates a [project](projects.md) named after the pack and puts every
resource in it, so the teardown removes what that project holds and never has to
guess by name.

## A pack

```yaml
{{#include ../examples/packs/acme-pack.yaml}}
```

| `pack:` | |
|---|---|
| `name` | The pack's name and its project's: letters, digits, `.`, `_`, `-` |
| `title`, `customer`, `notes` | Who it is for and what to remember; shown with the applied pack |
| `console_title` | The console's title while the pack is applied. The teardown puts back what was there |

A pack carries these sections and no others:

| Section | In a pack |
|---|---|
| `mocks` | The mocks, each with its spec inline (`spec_inline`). They go to the pack's project |
| `portal` | Portal items of the pack's own mocks (`mock:<name>`) |
| `tests` | Saved tests, in the pack's project |
| `test_schedules` | Schedules that name the pack's own tests (`names:`, no `prefix`) |
| `storyboards` | Storyboards, in the pack's project |

Anything else is refused by name: `settings`, `secrets`, `users`, `roles`, `projects`,
`registry`, `agents`, `harness`, `instance`, `health`. A pack never carries a secret
value: a test's or a storyboard's credential is a `{{VARIABLE}}`, as everywhere.

## Preview, then apply

Send the pack as the body, or as `{"document": ..., "dry_run": true}`. The preview
runs the same checks as the apply (the specs are compiled, the tests and storyboards
validated) and changes nothing:

```hurl
{{#include ../examples/packs/packs.hurl:preview}}
```

```hurl
{{#include ../examples/packs/packs.hurl:apply}}
```

The mock is deployed, the project holds everything, and the storyboard runs:

```hurl
{{#include ../examples/packs/packs.hurl:use}}
```

An apply is a unit:

- **A name that is taken is refused**, never overwritten: the project, a mock, a
  portal item, a saved test, a schedule or a storyboard of that name. The answer is
  `409` with `reason: name_taken` and the `collisions`.
- **A pack with a problem is refused whole** (`422`, `reason: invalid_pack`, the
  `problems`): a spec that does not compile, a test that does not validate. Nothing
  is left behind.

```hurl
{{#include ../examples/packs/packs.hurl:taken}}
```

```hurl
{{#include ../examples/packs/packs.hurl:refused}}
```

Applying needs `config.apply` and `users.manage` (a project is created), and
`settings.write` with `settings.security` for a `console_title`. At most 20 packs are
applied at once.

A pack from someone you do not vouch for is applied with `untrusted: true`
(`?untrusted=true`, `--untrusted`), which holds it to what a document from a
[repository](source-control.md) may do: a test or a storyboard step cannot point at a
gateway, and a schedule cannot name a webhook.

## What is applied

```hurl
{{#include ../examples/packs/packs.hurl:list}}
```

`holds` is what the pack's project holds now, which is what a teardown would remove.

## Tear it down

A dry run lists it first:

```hurl
{{#include ../examples/packs/packs.hurl:teardown_preview}}
```

```hurl
{{#include ../examples/packs/packs.hurl:teardown}}
```

The teardown removes what the project holds **now**: the mocks with their portal
items, the saved tests, the storyboards and any [checkpoint](checkpoints.md) saved in
the project, then the pack's schedules and the project itself. It follows the project,
not the document:

- a mock or test somebody added to the project during the demo goes with it;
- a resource somebody moved out of the project stays;
- the console title is put back only when it is still the one the pack set.

After a teardown an [export](configuration-as-code.md) of the instance is byte for byte
what it was before the apply.

## A demo built by hand becomes a pack

Any project exports as a pack: its mocks (specs inline) with their portal items, its
saved tests, the schedules that run only those tests, and its storyboards.

```hurl
{{#include ../examples/packs/packs.hurl:export}}
```

Build the demo in the console in a project, export it, and apply the file on the
instance you present from.

## From the command line

```bash
rustybin pack apply acme-pack.yaml --dry-run
rustybin pack apply acme-pack.yaml
rustybin pack list
rustybin pack export acme --output acme-pack.yaml
rustybin pack teardown acme --dry-run
rustybin pack teardown acme
```

`pack apply` also takes a directory: its `rustybin.yaml`, with each mock's `spec:`
file (a path inside the directory) read on your machine and sent inline. An instance
reads no files, so a pack sent to the API with a `spec:` path is refused.

```
acme-pack/
  rustybin.yaml        # pack: {name: acme}, mocks: [{name: acme-orders, spec: specs/orders.yaml}]
  specs/orders.yaml
```

An agent does the same with the [management MCP](agent-mcp.md) prompt
`prepare_customer_demo`: it previews the pack, applies it when you agree, rehearses
its storyboard and tells you how to tear it down.

## On every surface

| Surface | Where |
|---|---|
| API | `POST /_rustybin/packs/apply` (`dry_run`, `untrusted`; JSON, or the pack as `application/yaml`), `GET /_rustybin/packs`, `DELETE /_rustybin/packs/{name}` (`?dry_run=true`), `GET /_rustybin/packs/{name}/export` (`?format=json`) |
| Console | **Management**, **Packs**: choose a file or paste a pack, **Preview**, **Apply**; the applied packs with **What it holds**, export and **Tear down** (confirmed) |
| CLI | `rustybin pack apply <file|dir>` (`--dry-run`, `--untrusted`), `pack list`, `pack teardown <name>` (`--dry-run`), `pack export <project>` (`--output`, `--format`) ([reference](cli.md#pack)) |
| Management MCP | `apply_pack` (`dry_run`, `untrusted`), `list_packs`, `teardown_pack` (confirmed; `dry_run` is not), `export_pack`; the prompt `prepare_customer_demo` ([agent page](agent-mcp.md)) |
| `rustybin.yaml` | The `pack:` header marks a pack; an [export of the instance](configuration-as-code.md) never has one, and `POST /_rustybin/config/apply` does not take one |
