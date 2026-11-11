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
