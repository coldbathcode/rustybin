# Step 17: Secret requests, what step 12 left

## Goal

Three things step 12's build notes list as not built, each small, none changing the rule: a secret
is never a tool input or output, and only a person who may set a secret fulfils a request for it.

## A. A person hears about a waiting request

Today the card shows only inside Management. A signed-in person with `secrets.write` sees a count on
the gear from any view.

- The count comes from the route that already answers it (`GET /_rustybin/secret-requests`, its
  `pending`), never from `/_rustybin/status`, which answers callers who may not know requests exist.
- The console asks with its ordinary polling (`send`, never `EventSource`), only when the feature is
  on and the caller holds the permission, and stops when the tab is hidden.

## B. A note on a decline

- `POST /_rustybin/secret-requests/{id}/decline` takes an optional `note` (at most 300 characters,
  cleaned like the reason by `secret_requests::clean_reason`).
- The agent reads it in `get_secret_request` as `note`. It is a person's text, so it is not
  `untrusted_*`; the form says not to type the secret there, and a note is never in an audit entry.
- `rustybin settings decline <id> --note "..."`.

## C. The agent's own user sees its requests

Today a user whose agent asked sees the agent and not its request unless they hold `secrets.write`.

- The list and the single read answer a caller without `secrets.write` their own agents' requests
  (the rule of `agents::visible_to`), with the same fields.
- Such a caller may decline their own agent's request (it only takes away). Fulfilling keeps
  exactly what it needs today (`settings.write` and the key's group permission).
- This is a change of reach on four routes: the catalogue declares `.authenticated(&[])` with the
  body check listed in `rbac::need::BODY_CHECKS`, and `rbac::access_table` gets its rows.

## Surfaces

API, console (the count on the gear, the note field, the card for a user's own agents), management
MCP (`get_secret_request` answers `note`), CLI (`--note`), `rustybin.yaml` (nothing: a request is not
instance state).

## Tests that fail first

- A user without `secrets.write` lists only their own agents' requests, reads another's as `404`,
  declines their own, and is refused `403 permission_required` on fulfil.
- The note reaches the agent, is capped and cleaned, and a planted secret typed as a note is in no
  audit entry or log line (`nothing_secret_reaches_an_entry_or_a_file` grows a case).
- The browser check: the count shows outside Management, and goes when the request is resolved.

## Done when

The three are on every surface they touch, the parity and access tests pass, and
`docs/src/agent-mcp.md#a-secret-at-an-agents-request` describes them.

## Build notes (2026-11-12)

- **Where it is.** All three in `src/secret_requests.rs` (`visible_to`, `sees`, `clean_note`,
  `MAX_NOTE`, `Request.note`, the `decline` body; tests in `src/secret_requests/tests.rs`), the
  tool's answer in `src/mcp/manage/secrets.rs`, `--note` in `src/cli/commands.rs`, the console in
  `ui/lib/secretrequests.js` (the card, the note field, `pollCount` / `onCount` / `resetCount`)
  and `ui/app.js` (`paintGearCount`, the span `#secret-requests-count` inside the gear). User
  docs: `docs/src/agent-mcp.md` ("A secret at an agent's request", with the new heading "Your own
  agents' requests"), `docs/src/console.md`, `docs/src/cli-guide.md`, the body-check table of
  `docs/src/concepts/control-plane-security.md`; examples: the `decline`, `note` and `own`
  anchors of `docs/examples/agent/secret_requests.hurl`, `--note` in `docs/examples/cli/cli.sh`.
- **Deviation: the change of reach is on three routes, not four.** The list, the single read and
  the decline declare `.authenticated(&[])` with the body check (`secret_requests::visible_to`:
  every request with `secrets.write`, a user's own agents' without, anyone else refused naming
  `secrets.write`). **Fulfil keeps `.permission(Permission::SecretsWrite)`**: who may fulfil does
  not change, so its declaration does not either, and the route check stays the first thing a
  caller without `secrets.write` meets there (a user gets `403 permission_required:
  secrets.write` for their own agent's request as for anyone's). Declaring it `.authenticated`
  would have moved an unchanged rule from the catalogue into a handler for nothing.
- **Deviation: no `DELIBERATE` rows.** The four routes are in `NEW_SINCE_0_4` (step 12 put them
  there), which `the_old_scopes_reach_what_they_reached` leaves out: a `DELIBERATE` row for one
  fails as stale ("no longer a difference"). What the access table has is what its tests walk:
  `every_control_route_declares_a_permission` and `the_route_check_enforces_the_declarations`
  read the new declarations, and `rbac::need::BODY_CHECKS` has the new row (below). The reach
  itself is tested in `a_user_reads_and_declines_their_own_agents_requests_only` and
  `a_caller_who_is_no_user_still_needs_secrets_write`.
- **Whose request it is**: the user the asking agent's credential acts for
  (`Credential::owner()` of `Asker.credential`), the same fact `agents::visible_to` and
  `Agents::owner_of` use, read from the request so a request of an agent that is gone from the
  registry keeps its owner. An agent of the admin token or a control JWT acts for nobody: its
  requests are read with `secrets.write` only.
- **Another user's request is `404`** on the read and on the decline, whatever its state, with
  the body an unknown id gets (the test compares them), and the list leaves it out; `pending` in
  the list counts what the caller may read.
- **The note.** `{"note": "..."}` on the decline, optional (no body, `{}` and `null` are no
  note; a body that is not an object, or a note that is not a string, is `400` quoting nothing,
  and the request still waits). Cleaned by the reason's cleaner with its own cap (`clean_note`,
  `MAX_NOTE` 300): longer text is cut, not refused, as the reason is. Kept on a decline only;
  answered as `note` in the person's view and the agent's. The tool's text carries it too
  ("The person who declined left a note: ..."), since many clients show a model the text and not
  the structured content, and a note that never reaches the agent is no note. It is a person's
  text (someone with `secrets.write`, or the user the agent acts for), so not `untrusted_*`.
  Never in an audit entry (the entry has the request, the setting and the agent, as before) nor
  in a log line.
- **The count on the gear.** From `GET /_rustybin/secret-requests?state=pending` (`pending`),
  never from `/_rustybin/status`. Asked with `send` at the end of the console's existing 15 s
  status refresh and when the tab becomes visible; nothing is sent while the tab is hidden, when
  the caller may not ask (no sign-in on an instance that needs one, a visitor, an open control
  plane with an admin token this console does not hold) or after the route refused (401, 403,
  404: until the caller changes). **How "only when the feature is on" was built:** the console
  learns the switch from the same route's `enabled`; while it is off and nothing waits the route
  is asked once every two minutes (to notice it being turned on), not every 15 s. A meta tag or
  the status route would have told callers who may not know requests exist. The Management card
  updates the count from what it just read, so setting or declining there moves it at once.
- **Deviation: the count also shows a user their own agents' requests** (part C makes the route
  answer them), not only a caller with `secrets.write` as part A's sentence says: the card and
  the count would otherwise disagree for the same person.
- **The gear's accessible name stays "Console settings"** (the browser check and the keyboard
  path find it by that name); the count is the button's `aria-describedby` and its title.
- **Console, the card.** Shown to whoever the route answers. Decline is enabled for every row
  shown (the list holds only what the caller may decline); the value field and **Set the
  secret** stay off with the missing permission named, as before. The form says not to type the
  secret in the note.
- **Tests seen failing first**: `a_user_reads_and_declines_their_own_agents_requests_only`,
  `a_note_on_a_decline_reaches_the_agent_cleaned_and_capped`, the changed
  `fulfilling_needs_what_setting_that_secret_needs` and the new case of
  `nothing_secret_reaches_an_entry_or_a_file` failed before the code (the last one only because
  the note was not answered yet, so its protection was then removed once: a note put in the
  audit entry fails it with "ring holds a secret"). `a_caller_who_is_no_user_still_needs_secrets_write`
  passed before and after (it pins what must not move). The CLI case and the browser steps were
  written with the code.
- **An existing test changed, on purpose**: `fulfilling_needs_what_setting_that_secret_needs`
  asserted that a viewer is refused `403` on all four routes. A viewer is a user, so it now
  reads an empty list and gets `404` for another user's request; fulfil is still `403`.
- **Not built.** A browser check of a user without `secrets.write` seeing their own agent's card
  (the rule is tested in Rust and in the Hurl example; the browser check covers the count and
  the note as an admin). No push or live feed for the count: polling only. A note on a fulfil.

## For the merge

**`docs/src/changelog.md`**, "Unreleased".

Under **New**:

> - [A note on a declined secret request](agent-mcp.md#a-secret-at-an-agents-request):
>   `POST /_rustybin/secret-requests/{id}/decline` takes an optional `{"note": "..."}` (at most
>   300 characters, cleaned like the agent's reason), the console has a field for it beside
>   **Decline**, and `rustybin settings decline <id> --note "..."`. The agent reads it as `note`
>   in `get_secret_request`. Never type the secret there; a note is in no audit entry.
> - The console's gear shows how many secrets an agent asked for are waiting, from any view.

Under **Changed**:

> - **A change of reach**: [a user reads and may decline the secret requests of their own
>   agents](agent-mcp.md#your-own-agents-requests) without `secrets.write`
>   (`GET /_rustybin/secret-requests`, `GET /_rustybin/secret-requests/{id}`,
>   `POST /_rustybin/secret-requests/{id}/decline`, the card in **Management**,
>   `rustybin settings requests | decline`). Another user's request is `404`. Fulfilling is
>   unchanged: `settings.write`, `secrets.write` and the group's permission. A caller who is not
>   a user (a control JWT, an API key) still needs `secrets.write` for all of it.
> - `pending` in `GET /_rustybin/secret-requests` counts the requests the caller may read.
> - A request's answer has `note` (null unless a person left one on a decline), for the person
>   and for the agent.

**The change-of-reach note for the release notes**: before, the three routes above answered
`403 permission_required: secrets.write` to every caller without that permission. Now a signed-in
user without it gets `200` from the list (their own agents' requests, possibly none), and the
read and the decline of a request of their own agent; `404` for anyone else's. Nothing a user
could not already do to their own agent follows from it (they could revoke the agent), no secret
can be set through it, and callers who are not users see no change. A script that relied on the
`403` of the list to tell "may not read requests" should read `/_rustybin/session` instead.

**`CLAUDE.md`**, appended to the step 12 paragraph of "Core next steps 11 to 13" (or a line for
step 17 where the plan's later steps are listed):

> Step 17 (`docs/plans/core-next/step-17-secret-request-follow-ups.md`): every reader of secret
> requests on the control API goes through `secret_requests::visible_to` and `sees` (everyone's
> with `secrets.write`, a user's own agents' without, another's is `404`); owning the agent
> lets a user read and decline, never fulfil (the fulfil route keeps declaring `secrets.write`);
> a note on a decline is a person's text (`clean_note`, answered as `note`, not `untrusted_*`),
> never in an audit entry or a log line, and never a place for the secret; the gear's count
> comes from the requests route's `pending` (`ui/lib/secretrequests.js`: `pollCount`), never
> from `/_rustybin/status`, and is not asked while the tab is hidden.

Also the summary of `secrets.write` in `src/rbac/permission.rs` changed (it is shown wherever
permissions are listed).

**`docs/src/roadmap.md`**: remove whatever lists these three as under consideration (a note on a
decline, telling a person outside Management, a user's own agents' requests).

**`docs/plans/core-next/master-plan.md`**: step 17 done 2026-11-12.

**Shared tables this step touched** (a conflict there is two appends, keep both):
`rbac::need::BODY_CHECKS` (one row appended at the end), the `settings requests` and
`settings decline` rows of `COMMANDS` (`src/cli/commands.rs`, edited in place), the
`get_secret_request` description in `TOOLS` (`src/mcp/manage/tools.rs`, edited in place, no new
tool, so every tool count stays), the body-check table of
`docs/src/concepts/control-plane-security.md` (one row). Generated files to regenerate after the
merge rather than merge by hand: `README.md` (`RUSTYBIN_UPDATE_README=1 cargo test
readme_endpoints`) and `docs/src/cli.md` (`RUSTYBIN_UPDATE_CLI_DOCS=1 cargo test cli_docs`).
`conformance/ui/console_e2e.py`: `run_secret_requests` only.
