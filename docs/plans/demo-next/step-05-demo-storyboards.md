# Step 05: Demo storyboards

## Goal

A demo as a script: an ordered list of steps, each with a talk track, an action and a reset, that a
presenter clicks through in the console and can replay exactly the next time. A storyboard turns
"I hope the demo works" into a rehearsed, repeatable run.

## Where things stand

- The building blocks exist: checkpoints restore an instance to a known state
  (`src/checkpoints.rs`), gateway scenarios prove a policy (`src/scenarios/`), saved tests send a
  request and assert on it (`src/replay/`), the AI and fault headers shape an upstream on demand, and
  the health toggle breaks `/health`.
- Nothing strings them together with words for the presenter.

## Model

- `Storyboard { name, title, description, steps: [Step], project }`, at most 50 storyboards of at most
  40 steps, 64 KiB each, kept in `<state>/storyboards.json`.
- `Step { title, talk_track (Markdown, 4 KiB), action, expect, reset }`:
  - `action` is one of: `restore_checkpoint { name }`, `start_scenario { id, params }` and
    `check_scenario`, `run_test { name }`, `send { request }` (a `TestRequest`, through the gateway or
    in process), `set_health { up }`, `set_mock_mode { mock, mode }`, `open_view { route }` (the
    console only, so the audience sees the right page), or `none` (a talking point).
  - `expect` is the action's own verdict (a scenario's pass, a test's assertions), shown as a tick or
    a cross.
  - `reset` is an optional action run by "Reset" before a rehearsal.
- A run: the presenter's progress (current step, each step's result), in memory, one per session,
  bounded with the sessions; never instance state.

## Surfaces

| Surface | What changes |
|---|---|
| API | `/_rustybin/storyboards` (list, create), `/_rustybin/storyboards/{name}` (get, replace, delete), `/_rustybin/storyboards/{name}/steps/{index}/run`, `/_rustybin/storyboards/{name}/reset`; read with `tests.read`, write with `tests.write`, run a step with `tests.run` plus the permission of the step's own action (no new permission unless the step finds one is needed) |
| Console | A Storyboards view: an editor (steps, drag to reorder, the action form from the schema) and a presenter mode (full screen, the talk track, Next runs the action and shows the verdict, a keyboard shortcut per control) |
| Management MCP | `list_storyboards`, `save_storyboard`, `delete_storyboard`, `run_storyboard_step`, `reset_storyboard`; a prompt `rehearse_demo` |
| CLI | `rustybin storyboard list | show | apply | run <name> [--step N]` (`run` walks every step and prints the verdicts: a rehearsal in CI) |
| `rustybin.yaml` | A `storyboards:` section |

Docs: `docs/src/storyboards.md` with a worked example (break the upstream, show the retry policy,
restore), Hurl examples per route.

## Rules

- A step's action calls the same function as its own route or tool (never a control route in
  process), so a storyboard can do nothing its runner could not do directly, and each action checks
  the runner's permission at run time.
- A storyboard names checkpoints, tests and scenarios; it never embeds a secret (`{{NAME}}`
  variables only, as saved tests).
- A storyboard in a project is that project's resource (`project_api::resources` counts it).

## Tests that fail first

- A three-step storyboard runs through every surface with the same verdicts.
- A step whose action the runner lacks permission for is refused with `permission_required`.
- Reset restores the checkpoint named by the first step's reset.
- A document round trip (export, apply) keeps every field; a document from git is applied untrusted
  like the other sections.

## Done when

A storyboard can be written, rehearsed from the CLI and presented from the console, on every surface,
documented with tested examples.

## Build notes (2026-11-12)

Built whole. Where the build differs from the plan above:

- **There is no `expect` field.** A step's verdict is its action's own (a scenario's pass, a
  test's or a `send`'s assertions, a restore that worked), answered as `ok` with a `summary` and
  the action's answer in `detail`. A `send` action carries its assertions as `assert`, as a saved
  test does.
- **The action shapes**: `start_scenario { scenario }` takes no parameters (a scenario's
  parameters belong to its check), `check_scenario { scenario, gateway_url, params }` judges the
  run this storyboard's progress started (else the session's newest run of that scenario), and
  `set_health` takes `healthy`, the word the health route and `set_health` tool already use,
  not `up`. A `start_scenario` step run again ends its earlier run first, so a rehearsal leaves
  one run a scenario.
- **`set_mock_mode` deploys the mock's draft** (a mode that is only a draft changes nothing an
  audience sees), through `Svc::update` on the compile pool; a draft with other pending changes
  is deployed with them.
- **Steps are counted from 1** on every surface (`/steps/{index}/run`, `--step`, the tool's
  `step`).
- **A failed action is a verdict, a missing permission is a refusal.** `run::act` checks each
  action's permission itself and answers `403 permission_required` naming it before anything
  runs; any other failure (a checkpoint that is gone, a failed assertion, a refused gateway) is
  `200` with `ok: false`, so `rustybin storyboard run` goes on and reports every step.
- **Progress** is `Storyboards.runs`: one storyboard a session, 256 sessions, 12 hours since the
  last step, started over when the storyboard is saved again. The kept `detail` of a result is
  capped at 16 KiB and never holds a scenario's credential-injection key.
- **Permissions**: no new one. `tests.read`, `tests.write` and `tests.run` on the storyboard (in
  its project), plus the action's own at run time. The service handed to an action carries no
  route permission (`Caller.route_need` is cleared), or a mock in a project would be asked for
  `tests.run` instead of `mocks.write`.
- **The console editor** orders steps with up and down buttons, not drag (keyboard reachable,
  and nothing to add to the stylesheet's drag code). Its action forms are generated from the
  `storyboards` section of the configuration schema, so a new action kind needs no console
  change. `open_view` in presenter mode is a button that opens the route in a second tab (the
  overlay covers the console's own view).
- **The CLI** also has `storyboard reset` and `storyboard delete` (the routes need commands);
  `apply` is a `PUT`, the name from the argument or the file.
- **From git**: a storyboard is applied like a saved test (never moved between projects, never
  pointed at a gateway its saved copy does not name).
- **The MCP tools** confirm a step by what its action does (`run::confirm_reason`: a restore, a
  scenario check, a request through a gateway or with a method that changes state), and a failed
  step's summary goes under `untrusted_summary`, since it quotes what an upstream answered.
- The tests were written with the code, not before it: `src/storyboards/tests.rs` (the model,
  the store, the progress bounds, every action's verdict through the routes, the permission of
  each action for a tester and a viewer, the reset, the document round trip and the untrusted
  apply), `storyboard_tools_save_rehearse_reset_and_ask_before_what_changes_state`, the project
  fixture (`every_project_route_checks_each_resource` now reaches `sb` as a member of `a`), the
  pinned export and a console step in `conformance/ui/console_e2e.py`.

### For the merge

`src/storyboards/` (`mod.rs` the model, store, service and routes, `run.rs` the runner and the
progress, `tests.rs`), `AppState.storyboards`, `replay::run::run_these` (split out of `run`),
`storyboards:` in `MocksFile`, `configdoc` (export, diff, `apply_storyboards`, `Extra`), a
project's `Resources.storyboards`, `PROJECT_ROUTES` and `NEW_SINCE_0_4`, five tools at the end
of `TOOLS` (`PROJECT_TOOLS`, the frozen rights, 158 tools for an admin) and the prompt
`rehearse_demo` (ten prompts), the `storyboard` noun of the CLI, `ui/views/storyboards.js` and
the presenter styles at the end of `ui/styles.css`.
