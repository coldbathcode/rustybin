# Demo storyboards

A storyboard is a demo written down: an ordered list of steps, each with what the
presenter says, what the step does, and how to undo it. The presenter clicks through
it in the console; the same storyboard runs from the command line as a rehearsal, so
the demo that worked this morning is the demo the audience sees.

A step does nothing new. Its action is one of the things an instance already does
(restore a [checkpoint](checkpoints.md), start or check a
[gateway scenario](gateway-scenarios.md), run a [saved test](saved-tests.md), send a
request, flip the `/health` toggle, change a mock's response mode), run with the
permissions of whoever runs the step.

## Write one

Start from the state the demo begins in, saved as a checkpoint:

```hurl
{{#include ../examples/tests/storyboards.hurl:checkpoint}}
```

Then the storyboard. This one shows what a gateway does when its upstream goes down:

```hurl
{{#include ../examples/tests/storyboards.hurl:save}}
```

A storyboard has a `name` (letters, digits, `.`, `_`, `-`), an optional `title` and
`description`, and its `steps`. A step has:

| Field | |
|---|---|
| `title` | What the step is, in a line |
| `talk_track` | What to say, as Markdown (at most 4 KiB). The console's presenter mode shows it large |
| `action` | What **Next** does: an object with a `type` (below). Left out: a talking point |
| `reset` | An action a [reset](#reset-before-a-rehearsal) runs, to undo this step |

| Action | Does | Needs | The verdict |
|---|---|---|---|
| `none` | Nothing: a talking point | | Always a tick |
| `restore_checkpoint` with `name` | Restores the checkpoint: the API mocks, their data, the health toggle | `checkpoints.restore` | A tick when it restored |
| `start_scenario` with `scenario` | Starts a run of a gateway scenario and answers its upstream. Running the step again ends its earlier run first | `scenarios.run` | A tick when the run started |
| `check_scenario` with `scenario`, `gateway_url`, `params` | Checks the run this storyboard started (else your newest run of that scenario) through the gateway | `scenarios.run` | The scenario's verdict, with its expectations in `detail` |
| `run_test` with `name` | Runs the saved test; the run is kept in the test history with the trigger `storyboard:<name>` | `tests.run` on the test | The test's assertions |
| `send` with `request`, `gateway`, `assert` | Sends the request, in process or through `gateway`, as a saved test would (the same `request` and assertions) | | The assertions |
| `set_health` with `healthy` | Sets the `/health` toggle | `settings.write` | A tick |
| `set_mock_mode` with `mock`, `mode` | Sets the mock's response mode (`examples`, `generated`, `static`, `stateful`) and deploys its draft | `mocks.write` on the mock | A tick when it deployed |
| `open_view` with `route` | Names a console page to show (`#/requests`, `#/mocks/orders`). Presenter mode gives a button that opens it in a second tab, the one the audience watches; the other surfaces answer the route | | Always a tick |

At most 50 storyboards of 40 steps, 64 KiB each, kept in `<state>/storyboards.json`
(in memory without a state directory). Reading one needs `tests.read`, saving and
deleting `tests.write`.

**A credential is never saved.** A `send` step's secret-looking header or query value
references a variable (`Authorization: Bearer {{TOKEN}}`), as in a saved test; a
literal one is refused. A run gives the values (`variables`, else the instance's
`RUSTYBIN_TEST_*` environment variables), and no result repeats one.

```hurl
{{#include ../examples/tests/storyboards.hurl:list}}
```

## Run a step

Steps are counted from 1.

```hurl
{{#include ../examples/tests/storyboards.hurl:step}}
```

The answer is the verdict: `ok`, a `summary` in a line, the action's own answer in
`detail` (a restore's changes, a test's result, a scenario's expectations), and `next`,
the step after this one (`null` after the last).

```hurl
{{#include ../examples/tests/storyboards.hurl:verdicts}}
```

Running a step needs `tests.run` on the storyboard **and** what its action needs.
Someone who lacks it gets `403` with `reason: permission_required` and the missing
`permission`, and nothing runs: a storyboard never lends its author's rights to its
runner.

A step that fails for any other reason is still an answer, with `ok: false` and why.
A rehearsal therefore goes on to the next step and reports every verdict:

```hurl
{{#include ../examples/tests/storyboards.hurl:failed}}
```

## Progress

An instance remembers where each presenter is: the step they are at and every step's
last result, per session (`X-Rustybin-Session`, else the client address).

```hurl
{{#include ../examples/tests/storyboards.hurl:progress}}
```

Progress is a convenience, never state of the instance: it is kept in memory for one
storyboard a session, for 12 hours since the last step, for at most 256 sessions,
and it starts over when the storyboard is saved again. It is in no export and no
checkpoint.

## Reset before a rehearsal

A reset runs every step's `reset` action in order, ends the scenario runs your
progress started, and clears your progress. It needs what a step needs: `tests.run`
and the permission of each reset action.

```hurl
{{#include ../examples/tests/storyboards.hurl:reset}}
```

Give the first step a `reset` that restores the demo's checkpoint and every reset
ends in the same state.

## Present it

In the console, **Storyboards** lists them. **Edit** opens the editor: the title, the
steps in order (the arrows move a step), each step's talk track, action and reset.
**Present** opens presenter mode, full screen: the talk track of the step you are at,
**Next** to run its action and see the tick or the cross with the summary, and the
list of steps down the side with each one's last verdict.

| Key | |
|---|---|
| `→` or `Space` | Run this step's action and move to the next |
| `←` | Back one step, without running anything |
| `R` | Run this step again |
| `Esc` | Leave presenter mode |

**Reset** in the list or in presenter mode is the reset above, after a confirmation.

## Rehearse it

```bash
rustybin storyboard run upstream-down
rustybin storyboard run upstream-down --step 3
rustybin storyboard reset upstream-down
```

`run` walks every step in order and prints each verdict; it exits `1` when a step
fails, so a rehearsal fits a CI job before a demo day. `--var NAME=value` gives a
variable; a credential comes from stdin (`--var TOKEN=-`) or a file
(`--var TOKEN=@token.txt`), never the command line.

An agent does the same with the [management MCP](agent-mcp.md) prompt
`rehearse_demo`: it resets, runs each step, asks you before a step that restores a
checkpoint, sends traffic through a gateway or replays a request that changes state,
and reports the verdicts.

## Change and delete

`PUT` replaces a storyboard (or creates it); `DELETE` removes it. What its steps
changed on the instance stays as it is.

```hurl
{{#include ../examples/tests/storyboards.hurl:replace}}
```

```hurl
{{#include ../examples/tests/storyboards.hurl:cleanup}}
```

## In a project

A storyboard with `project` belongs to that [project](projects.md): its members read,
save and run it with their roles there, others do not see it, and deleting the
project moves or refuses it with the project's mocks and tests. Each action is still
checked on what it touches: a step that restores another project's checkpoint needs
`checkpoints.restore` in that project.

## In `rustybin.yaml`

```yaml
storyboards:
  - name: upstream-down
    title: What the gateway does when the upstream goes down
    steps:
      - title: A known starting state
        talk_track: Every run starts from the **same** mocks.
        action: { type: restore_checkpoint, name: demo-start }
        reset: { type: restore_checkpoint, name: demo-start }
      - title: Break the upstream
        action: { type: set_health, healthy: false }
        reset: { type: set_health, healthy: true }
      - title: Through the gateway
        action:
          type: send
          gateway: https://gateway.example.com
          request:
            path: /orders/1
            headers: { x-api-key: "{{GATEWAY_KEY}}" }
          assert: [{ status: 503 }]
      - title: Questions
```

Applying a document saves the storyboards and runs nothing. With `prune`, storyboards
the section does not name are deleted; without the section they are left alone. A
document from the [configuration repository](source-control.md) cannot move a
storyboard to another project, and cannot point a step at a gateway its saved copy
does not already name: such a storyboard is skipped and reported.

## On every surface

| Surface | Where |
|---|---|
| API | `GET`, `POST /_rustybin/storyboards`, `GET`, `PUT`, `DELETE /_rustybin/storyboards/{name}`, `POST /_rustybin/storyboards/{name}/steps/{index}/run`, `POST /_rustybin/storyboards/{name}/reset` |
| Console | **Storyboards**: the list, **New storyboard**, **Edit** (steps, order, talk track, action and reset forms), **Present** (presenter mode with the keys above), **Reset** and **Delete** (confirmed) |
| CLI | `rustybin storyboard list`, `show <name>`, `apply [<name>] --file F`, `run <name>` (`--step N`, `--var NAME=value`; exits `1` when a step fails), `reset <name>`, `delete <name>` ([reference](cli.md#storyboard)) |
| Management MCP | `list_storyboards` (all, or one with your progress), `save_storyboard`, `delete_storyboard` (confirmed), `run_storyboard_step` and `reset_storyboard` (confirmed when an action restores a checkpoint, sends through a gateway or replays a request that changes state; variables only from `RUSTYBIN_TEST_*`); the prompt `rehearse_demo` ([agent page](agent-mcp.md)) |
| `rustybin.yaml` | `storyboards:`, a list as above ([instance as code](configuration-as-code.md)); progress is never exported |
