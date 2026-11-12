# Changelog

## Unreleased: the last of the roadmap's candidates

[The plan](https://github.com/coldbathcode/rustybin/blob/main/docs/plans/core-next/master-plan.md),
steps 11 to 13, built on 2026-11-09 and not released yet, and steps 01 to 09 of
[the demo next plan](https://github.com/coldbathcode/rustybin/blob/main/docs/plans/demo-next/master-plan.md),
built on 2026-11-11 and 2026-11-12. Nothing to do when upgrading: what is new is off until you turn it on or
name it in a scenario.

**New**:

- [AI gateway scenarios](gateway-scenarios.md#ai-gateway-scenarios): six guided scenarios with a
  pass or fail verdict for an AI gateway in front of this instance's AI mock: `ai_token_limit`
  (a token budget, told apart from a request limit), `ai_model_fallback` (a provider error on the
  primary target, the success from a fallback; `fail_kind`), `ai_semantic_cache` (rewordings from
  the cache, a different prompt to the model), `ai_prompt_guard` (fake personal data and a prompt
  injection refused or redacted; `skip_injection`), `ai_response_guard` (a fake customer record
  removed from the answer) and `ai_provider_balance` (two targets, each its share; `min_share`).
  A run steers the AI mock for its own requests only (the run's secret session), and ending it
  ends that. On every surface: the scenario routes, the console's AI group, `rustybin scenario`
  (`--token-limit`, `--fail-kind`, `--skip-injection`, `--min-share`), the scenario tools.
- [MCP security](mcp/security.md): attack servers at `/mcp/attack/{kind}` that misbehave on
  purpose with inert test data (tool poisoning, a rug pull, tool shadowing, result injection, an
  oversized result, an exfiltration link, scope overreach), on by default and always off in public
  mode (`mcp.attack_servers`, `RUSTYBIN_MCP_ATTACK_SERVERS`); six MCP gateway scenarios against
  them (`mcp_tool_poisoning`, `mcp_rug_pull`, `mcp_tool_shadowing`, `mcp_result_injection`,
  `mcp_oversized_result` with `max_result_kib`, `mcp_exfiltration_url`), each check an MCP
  session through the gateway; and three inspector checks that warn on any server
  (`descriptions_look_clean`, `results_carry_no_instructions`, `definitions_are_stable`). The
  attack servers are listed in the MCP inspector, never in the mock MCP registry.
- [A mock recorded from a real API](mocks/record.md): `POST /_rustybin/recordings` answers a
  proxy URL; requests sent to it go to the real API through the outbound policy (purpose
  `record`, no default) and the exchanges are kept with credentials removed (headers not kept,
  secret-looking query values and JSON fields as `{{NAME}}`); `POST .../build` infers a spec by
  code (path templates, parameters and schemas from the traffic) and creates the mock with the
  recorded answers as stored examples (source `recorded`), naming what looked like personal data
  by kind. On every surface: the routes, `rustybin record`, six management tools, the console's
  Mock APIs view.
- [Contract drift](mocks/drift.md): `POST /_rustybin/mocks/{name}/drift` sends one request per
  operation to the real API a mock imitates (`drift.upstream`, a new setting of a mock, or
  `upstream`) under the purpose `record`, and reports `status_differs`, `schema_violation`,
  `missing_field`, `undocumented_field` (a warning) and `unreachable` per operation, as JSON or
  JUnit; credentials as `{{VAR}}` headers, never kept; the last 10 runs kept per mock. On every
  surface: `rustybin mocks drift` (exits 1 on drift) and `drift-runs`, `check_mock_drift` and
  `list_drift_runs`, a mock's Drift tab, `drift:` in `rustybin.yaml`.
- [Demo storyboards](storyboards.md): a demo as a script. A storyboard is a list of steps, each
  with a talk track (Markdown), an action and an optional reset; an action is what an instance
  already does (`restore_checkpoint`, `start_scenario`, `check_scenario`, `run_test`, `send`,
  `set_health`, `set_mock_mode`, `open_view`, or `none` for a talking point). `POST
  /_rustybin/storyboards/{name}/steps/{index}/run` runs one step with the caller's own
  permissions (`tests.run` plus what the action needs; a missing one is `403
  permission_required` and nothing runs) and answers the verdict; `POST .../reset` runs every
  step's reset, ends the scenario runs the storyboard started and clears the caller's progress.
  Progress is per session, in memory, never exported. At most 50 storyboards of 40 steps in
  `<state>/storyboards.json`; a credential is never saved (`{{NAME}}` variables, as saved tests).
  A storyboard may belong to a project. On every surface: the routes, the console's Storyboards
  view (an editor whose action forms come from the configuration schema, and a presenter mode
  driven from the keyboard), `rustybin storyboard list`, `show`, `apply`, `run` (a rehearsal:
  exits 1 when a step fails), `reset`, `delete`, five management tools and the prompt
  `rehearse_demo`, `storyboards:` in `rustybin.yaml` (a pulled document cannot point a step at
  a new gateway).
- [Customer demo packs](demo-packs.md): a customer's whole demo as one document, a
  `rustybin.yaml` with a `pack:` header (`name`, `title`, `customer`, `notes`, `console_title`)
  and the sections `mocks`, `portal`, `tests`, `test_schedules` and `storyboards` only. `POST
  /_rustybin/packs/apply` creates a project named after the pack and applies the document into
  it (`dry_run` previews after the same checks; a name that is taken is `409 name_taken`, never
  overwritten; a pack with a problem is refused whole); `DELETE /_rustybin/packs/{name}` removes
  what the project holds now, the pack's schedules and the project, and puts the console title
  back (`dry_run` lists it), leaving an export of the instance byte for byte what it was; `GET
  /_rustybin/packs/{name}/export` gives any project as a pack. At most 20 packs. On every
  surface: the routes, Management, Packs in the console, `rustybin pack apply` (a file, or a
  directory whose spec files it sends inline), `list`, `teardown`, `export`, four management
  tools and the prompt `prepare_customer_demo`.
- [Multi-step saved tests](saved-tests.md#multi-step-tests): a test takes `steps` instead of
  `request`, each step a request with its assertions and `outputs` (`$statusCode`,
  `$response.header.<name>`, `$response.body#/json/pointer`) that later steps use as
  `{{steps.<id>.outputs.<name>}}`. At most 20 steps; a step that fails stops the test and is
  named in the failure; a result has each step with its outputs (one named like a credential is
  hidden). The Hurl export writes a step as an entry and an output as a capture.
- [Arazzo workflows](saved-tests.md#arazzo-workflows): `POST /_rustybin/tests/import` reads an
  Arazzo 1.0 document and makes one multi-step test a workflow, its source descriptions being the
  instance's mocks (by name, else the one mock with the operations used; never fetched). What has
  no counterpart (an `onFailure` goto or retry, regex and XPath criteria, nested workflows) is
  named in `left_out`, never guessed; `dry_run` previews. `GET
  /_rustybin/tests/{name}/export?format=arazzo` writes a test back as Arazzo. The harness import
  reads the same document (`format: arazzo`) for the steps that stand alone. On every surface:
  the console's Import Arazzo card and per-step results, `rustybin tests import` and `tests
  export <name> --format arazzo`, the tool `import_tests`, `steps:` in `rustybin.yaml`.
- [Realtime API](ai/realtime.md) of the mock LLM: `GET /ai/openai/v1/realtime`, the OpenAI-shaped
  realtime protocol over a WebSocket (`session.update`, `conversation.item.create`,
  `input_audio_buffer.*`, `response.create` and `cancel`; the `response.*` events with text,
  audio and transcript deltas, function calls from the session's tools, `response.done` with
  usage). Answers come from the engine of the other AI paths. Input audio is counted and never
  kept; output audio is silence or a tone in PCM16 or G.711. `?fail=drop_after:N` and
  `error_after:N` fault a session, a fail kind refuses the upgrade. At most
  `ai.realtime.max_sessions` (64) sessions of `ai.realtime.session_minutes` (30).
- [Batch APIs](ai/batch.md) of the mock LLM: OpenAI-shaped files and batches
  (`/ai/openai/v1/files*`, `/ai/openai/v1/batches*`) and Anthropic-shaped message batches
  (`/ai/anthropic/v1/messages/batches*`). A batch moves through the provider's states in
  `X-Rustybin-Batch-Seconds` (default 10, `ai.batch.seconds`); its results are the engine's and
  deterministic for the same input, ids included; `X-Rustybin-Batch-Fail-Rate` fails chosen
  lines with the provider's error, a malformed line fails validation, a cancel goes through
  `cancelling`. Per caller `ai.batch.max_files` (20) and `ai.batch.max_batches` (10), an hour.
- The AI request log (`GET /ai/requests`) has a `kind` (`request`, `realtime`, `batch`); a
  realtime session's record holds its events and a batch's its lines, never audio. The console's
  AI playground has a Realtime session panel and a Request log card.
- [Event mocks from AsyncAPI](mocks/asyncapi.md): an AsyncAPI 2.x or 3.0 document becomes a mock
  with channels in place of operations, in the same store as the OpenAPI mocks (drafts, deploys,
  revisions, projects, exports). Each channel answers at `/events/{mock}/{channel}` as a
  WebSocket, as server-sent events and to a `POST`; Rustybin publishes the document's `send`
  messages (its examples, the mock's stored ones, or generated from the schema) on a pattern
  (`every`, `burst`, `on_receive`, `off`), with faults (`delay_ms`, `drop_percent`,
  `malformed_percent`), and validates what clients publish against the `receive` messages: an
  invalid one is answered with an error and kept with its report. Webhook subscribers
  (`/_rustybin/mocks/{name}/subscribers`) get a channel's messages posted, under the new
  outbound purpose `events`, which has no default. The deploy report names what is not served
  (`kafka`, `mqtt` and other bindings) or not generated (Avro, Protobuf). On every surface: the
  console's Channels tab, `rustybin mocks channels | events | publish | subscribers | subscribe |
  unsubscribe`, six management tools, `channels:` of a mock in `rustybin.yaml`, and the
  settings `asyncapi.max_connections` (128), `asyncapi.session_minutes` (60) and
  `asyncapi.max_subscribers` (50). Kafka is not built.

- Source control: a [diverged branch](source-control.md#a-diverged-branch) says what differs
  (`divergence` in the status and in the `409`) and is resolved by a person: `POST
  /_rustybin/git/resolve` takes `instance`, `repository` or `pull_request` (this instance's
  side to a new branch, for a pull request at the git host), each with `dry_run`. Still never a
  merge, never a forced push, and the automatic mode never resolves.
- [Branches per environment](source-control.md#branches): `GET /_rustybin/git/branches`, and
  `POST /_rustybin/git/branch` to switch with a preview or to start a branch from the instance.
  `rustybin git branches`, `switch`, `branch`; the tools `git_branches`, `git_switch_branch`.
- [Signed commits](source-control.md#signed-commits): `git.sign` (off by default) with a signing
  key of its own, the secret `git.signing_key`; `POST /_rustybin/git/signing-key` generates it and
  answers the public key and its fingerprint. `rustybin git signing-key`, the tool
  `git_signing_key`. The instance signs; it does not verify the signatures of what it pulls.
- A saved harness scenario [in a file of its own](configuration-as-code.md#a-scenario-in-a-file):
  `file:` in the `harness` section, read like a mock's `spec:`.
- [A secret at an agent's request](agent-mcp.md#a-secret-at-an-agents-request): with the
  `agents.secret_requests` setting on (off by default), an agent asks for a secret setting to
  be set (`request_secret`: the setting and a reason) and a person types the value in
  **Management** or gives it to `rustybin settings fulfil <id>` on stdin, or declines. The value
  never passes through the agent: the tools answer a request's state only. Requests expire after
  15 minutes; `GET /_rustybin/secret-requests`, `POST .../{id}/fulfil | decline`
  (`secrets.write`, and what setting that secret needs).
- [An agent's sign-in from the command line](agent-mcp.md#connect-an-agent): `rustybin login
  --agent` walks the instance's own OAuth sign-in in the browser and keeps the agent's tokens in
  the profile; `rustybin mcp-stdio --profile <name>` uses and renews them. The credential works
  with `mcp-stdio` only, and the other commands say so.
- Harness: [import from other tools](harness.md#importing-a-postman-collection-or-an-artillery-script).
  `POST /_rustybin/harness/import`, `rustybin harness import --format` and the new
  `import_scenario` tool read a Postman collection (v2.1) and an Artillery script as well as a
  recording. What has no counterpart (scripts, hooks, plugins, captures, think times) is never run
  and is named in `left_out`; a request with a variable the document does not define is left out
  and the variable named; an Artillery script's phases become `pattern` when they map exactly. A
  program is refused.
- Harness: [HTTP/3 targets](harness.md#http3-targets) (`targets[].http: h3`) in builds with the
  cargo feature `harness-http3`. The default binary and the published image have no QUIC stack and
  refuse the scenario with a message naming the feature. QUIC connects to the run's pinned
  addresses only.
- Harness: [server-to-server chains](harness.md#server-to-server-chains) (`servers.chain`): a
  server answering a request of the run calls a further hop, and the report gains the leg of each
  hop (`server.chain`). A server calls on only for a request with its run's signed marker, only
  while the run is live, only to a hop the scenario names and the server node allows, pins and
  verifies, once per request and hop, to a depth of 3. A scenario with a chain needs every server
  node on this version; runs without one are unchanged on a mixed cluster.

**Changed**:

- The management MCP server has 163 tools for an admin (`request_secret`, `get_secret_request`,
  `git_branches`, `git_switch_branch`, `git_signing_key`, `import_scenario`, the six recording
  tools `list_recordings`, `start_recording`, `get_recording`, `stop_recording`,
  `build_mock_from_recording`, `delete_recording`, `check_mock_drift`, `list_drift_runs`, and
  the five storyboard tools `list_storyboards`, `save_storyboard`, `delete_storyboard`,
  `run_storyboard_step`, `reset_storyboard`, and the four pack tools `list_packs`, `apply_pack`,
  `teardown_pack`, `export_pack`, and `import_tests`) and eleven prompts (`rehearse_demo` and
  `prepare_customer_demo` are new).
- A saved test's name cannot be `import` (a route now). A test's result has `steps` when it is
  a multi-step test; a single-request test is as before.
- `rustybin.yaml` has a `storyboards:` section (empty when there are none), and a project's
  resources count its storyboards.
- `profiles.json` may hold an `agent` object per profile. An older `rustybin` reads the file
  and ignores it.
- A harness import keeps at most 64 requests (a layer's limit; it kept 200, which did not
  validate) and a weight never passes 1000.

**Fixed**:

- A document pulled from the configuration repository could change the `git.*` settings (the
  remote, the branch, the automatic mode) when someone wrote them into it by hand. It no longer
  can: a different value is a problem and the setting is kept.
- A mock whose spec changed, and nothing else, was not seen as a change against the working
  copy: `git.auto: push_on_change` did not commit it and the status showed no drift.
- A harness scenario written by hand in a document (the shorthand with `target`) was refused
  when applied; it is folded like every other way of saving one.

## 0.6.0 (2026-11-07): more from an instance

[The plan](https://github.com/coldbathcode/rustybin/blob/main/docs/plans/core-next/master-plan.md),
steps 01 to 10, from the roadmap's candidates. Nothing to do when upgrading; three things are new
and off until you turn them on.

**New**:

- [A spec from a URL](mocks/index.md#a-spec-from-a-url): `spec_url` on a create, `--spec-url`, and
  **Or fetch it** in the new mock dialog. It needs `mocks.write` and a host named in
  `RUSTYBIN_EGRESS_ALLOW` (purpose `spec_fetch`, no default), so never a visitor's.
- [A push webhook](source-control.md#a-webhook-instead-of-polling) for source control:
  `POST /_rustybin/git/webhook` pulls at once, signed, and answers `404` until
  `git.webhook_secret` is set.
- [OAuth on a mock's own MCP server](mocks/mcp.md#security): `mcp.oauth` answers `401` with protected
  resource metadata, to try a client's sign-in.
- [Stateful visitor mocks](mocks/self-service.md), behind
  `RUSTYBIN_MOCKS_SELF_SERVICE_STATEFUL`, at most 50 items and 64 KiB per mock. Off by default.
- For the harness: [a leader that steps aside](harness.md#failover) before maintenance with the run
  still on; [a recording as a scenario's source](harness.md#replaying-a-recording), from a HAR file or
  saved captures, without its credentials; and [a finished run's metrics](harness.md#opentelemetry) to
  an OpenTelemetry collector.
- `rustybin egress check <url>` and `GET /_rustybin/egress/check`: what the outbound policy would say
  to a URL, without calling it. Also **Check a URL** on the Network tab.
- Suggestions for the style of generated examples, and **Generate more** on an operation.

**Changed**: `GET /_rustybin/mocks/export` writes a version 2 document with only the `mocks` section.
It applies as before.

`cargo` version 0.6.0, the tag `v0.6.0`, the signed multi-arch image
`ghcr.io/coldbathcode/rustybin:0.6.0`, digest
`sha256:492000348f22ebb73cd6197de2a07ea6bfc9a363b606b33dcf25b535e29cceb3` (verify it with the
cosign command of the [quick start](quick-start.md#docker); the
[release](https://github.com/coldbathcode/rustybin/releases/tag/v0.6.0) carries the same
values, and the Linux and macOS binaries).

## 0.5.2 (2026-11-07): an operator's sign-in next to anonymous visitors

**Changed**: `RUSTYBIN_MOCKS_SELF_SERVICE=true` now works with the password sign-in, where
it was ignored with a warning: visitors stay anonymous and only the operator signs in. See
[An operator who signs in](mocks/self-service.md#an-operator-who-signs-in). **Check this
before upgrading**: an instance that has `RUSTYBIN_MOCKS_SELF_SERVICE=true` set, public
mode and `password` control auth was locked and now takes visitors. Remove the variable to
keep it locked. `token` and `jwt` still ignore it.

**New**: `rustybin password hash` makes the argon2id hash for
`RUSTYBIN_CONSOLE_PASSWORD_HASH` on your own machine.

**Fixed**: a harness cluster's failover could pass over the standby. Nodes took their turn
to stand for election in slots narrower than the gap between their heartbeats, so a later
node could stand first and `RUSTYBIN_HARNESS_LEADER=prefer` meant little. The first in line
stands as before, after the election timeout, and the next ones now wait half of it each.

`cargo` version 0.5.2, the tag `v0.5.2`, the signed multi-arch image
`ghcr.io/coldbathcode/rustybin:0.5.2`, digest
`sha256:391f066e845ee0eb9030cc705b19fe2a068077533c0a1a88afbb0abc262ad2dd` (verify it with the
cosign command of the [quick start](quick-start.md#docker); the
[release](https://github.com/coldbathcode/rustybin/releases/tag/v0.5.2) carries the same
values, and the Linux and macOS binaries).

## 0.5.1 (2026-11-06): safe for a public demo

No new features: what running the public demo's configuration on 0.5.0 found, fixed
([the plan](https://github.com/coldbathcode/rustybin/blob/main/docs/plans/hosted-demo/master-plan.md),
steps 01 to 03). Upgrade any instance in public mode: the first two entries below are
open on 0.5.0 and earlier. Behind a proxy, also read the third.

**Security**: on a public instance, or to an API key in shared mode, the captured
requests of an instance mock (the built-in `petstore`, for example) were readable by
any other caller, with the `Authorization` and `Cookie` headers and the query string
the sender used. A caller without a credential now reads only its own session's calls
and a key only its own, on the requests list, one request by number, the live feed,
the rule tester and the `get_mock_traffic` and `wait_for_requests` tools; `total` then
counts those calls. The operator, signed-in users and a visitor's own mocks are
unchanged. An operator of a public instance on 0.5.0 or earlier should treat
credentials sent to its instance mocks as seen.

**Security**: one anonymous `POST /_rustybin/mocks` could take a self-service instance
down. A 160 KiB spec whose operations all name one wide schema took about 645 MiB,
because each MCP tool kept its own expansion of the schema and the deploy report its
own generated body; two such specs, or about 100 large ordinary ones, ended a 512 MiB
container. A visitor's tools now expand at most 32,000 schema nodes per revision
(1,000,000 otherwise), a report keeps 256 KiB of generated bodies with `body_omitted`
on the rest (an operation's detail still generates its whole sample), and all visitor
mocks share a memory budget, `RUSTYBIN_MOCKS_SELF_SERVICE_MAX_MB` (192 MiB by default):
past it a new mock or an edit answers `503`. The same spec now costs about 18 MiB.

**Security**: a public instance with `RUSTYBIN_TRUST_FORWARD` and no
`RUSTYBIN_TRUST_FORWARD_HEADER` believes every forwarding header, so a client could
name its own address and step around the per-address limits. It now warns at startup and
in `GET /_rustybin/diagnostics` (`client_address`). Nothing changes until you name your
proxy's header: see [Behind a proxy](mocks/self-service.md#behind-a-proxy).

**Changed on a public instance**: `GET /_rustybin/metrics` needs `settings.read` (set
`RUSTYBIN_ADMIN_TOKEN` and scrape with it, or nobody reads them), and a visitor's `mcp-checks` runs are answered but not kept, nor are the saved runs shown to
them (they were one list, so visitors saw each other's targets and could push them out).
The console's Saved tests view says once that the tests are the operator's, in place of
three errors.

`cargo` version 0.5.1, the tag `v0.5.1`, the signed multi-arch image
`ghcr.io/coldbathcode/rustybin:0.5.1`, digest
`sha256:25026bdff9bd84fa146187c410962b722e04bf9a422ea78781dfe95d4d9bb7b6` (verify it with the
cosign command of the [quick start](quick-start.md#docker); the
[release](https://github.com/coldbathcode/rustybin/releases/tag/v0.5.1) carries the same
values, and the Linux and macOS binaries).

## 0.5.0 (2026-11-03): access control

Several people on one instance, each with the roles they need: users with roles
made of named permissions (built-in and custom), invitations instead of shared
passwords, single sign-on with any OpenID Connect provider, projects so teams share
an instance, AI agents that act for a user and never above them, APIs on the dev
portal that are open or closed behind a portal sign-in, and an audit log of who did
what. A security review of the whole access-control build closed 33 findings before
the release
([the plan](https://github.com/coldbathcode/rustybin/blob/main/docs/plans/testing-2/master-plan.md),
steps 07 to 15).

**Upgrading from 0.4** (nothing to do for most instances; what an operator or a
script may notice):

- The console user becomes the first user, with the `admin` role, at the first
  start; its password stays where it was. Sign-in tokens from 0.4 keep working as
  that user until they expire.
- The old control scopes keep their 0.4 reach as roles (`console`, `inspector`,
  `harness`, `admin`); a control JWT may also carry `rb_roles` and `rb_perms`.
- A refusal is `403 {"error", "reason": "permission_required", "permission"}` naming
  what is missing (was `insufficient_scope`); the CLI prints it and exits `3`.
- One admin's password change ends that admin's sessions and agents only, no longer
  everyone's; `DELETE /_rustybin/sessions` (sign out everyone) needs every
  permission.
- Every published portal item stays open. `portal.auth=password` now asks for a
  portal sign-in by a user holding `portal.read` (the `portal_reader` role gives
  only that) instead of keeping a console token. A mock closes only behind
  credentials of its own: a mock closed behind the default `demo-key` answers `403
  closed_unsecured` until it is secured again (`secure: true`).
- Single sign-on links a user created ahead only through an email the provider
  verified.
- The `git.*` settings need `git.write`; `update_settings` needs what `PATCH
  /_rustybin/settings` needs; an `editor`'s export carries no users, roles or
  projects; a `/_rustybin/*` route missing from the catalogue answers `403`.
- The management MCP server has 137 tools for an admin (118 without the harness
  ones); clients that read only the first `tools/list` page use `?page_size=200`.

The entries below are each step's, with their own behaviour changes.

`cargo` version 0.5.0, the tag `v0.5.0`, the signed multi-arch image
`ghcr.io/coldbathcode/rustybin:0.5.0`, digest
`sha256:70baf467a6be5faca3a85655719cb97045ffe610d0a4f63d12a697eef0470135` (verify it with the
cosign command of the [quick start](quick-start.md#docker); the
[release](https://github.com/coldbathcode/rustybin/releases/tag/v0.5.0) carries the same
values, and the Linux and macOS binaries).

- Users, roles and permissions, the model (access control, step 07 of the
  [plan](https://github.com/coldbathcode/rustybin/blob/main/docs/plans/testing-2/master-plan.md)):
  a user store (`<state>/users.json`), 38 named permissions, the built-in roles
  `viewer`, `tester`, `editor`, `admin` and `portal_reader`, custom roles. On the
  first start the console user becomes user one with the `admin` role (nothing to
  do; its password stays in the sign-in settings and the environment). Sign-in
  tokens carry the user id (`sub`), `rb_user`, `rb_gen` and `rb_ugen`, and resolve
  their permissions from the user at every request; tokens issued before the upgrade
  stay valid as user one. Control JWTs may carry `rb_roles` and `rb_perms` (unknown
  names ignored). `GET /_rustybin/session` answers `user_id`, `display_name`, `roles`
  and `permissions`; the sign-in answer and the sessions list carry `user_id`. What
  each credential may do is unchanged until every route checks a permission (step
  08). A new diagnostics check, `users`.
- Every route and tool checks a permission (access control, step 08): each control
  route declares the permission it needs per method in the catalogue (or that it is
  open, or needs only a signed-in caller), one check makes the decision before the
  handler runs, and each management tool declares one too; the yes/no admin check is
  gone. A refusal is `403 {"error", "reason": "permission_required", "permission"}`
  naming what is missing. The CLI prints `permission_required: <permission>` and exits
  `3`, and its reference has a **Needs** column; the console asks `GET
  /_rustybin/session` and disables what the caller may not do, saying which permission
  it needs; the agent tool tables have a **Permission** column. `GET
  /_rustybin/settings` answers what changing each setting `needs`.

  **Behaviour changes** (the 0.4 scopes keep their reach, `console` as the `viewer`
  role plus `traffic.read`, `assist.use` and `mcp.inspect`, `harness` as every
  `harness.*` permission; every difference is listed in
  [the step's build notes](https://github.com/coldbathcode/rustybin/blob/main/docs/plans/testing-2/step-08-permissions-everywhere.md)):
  - a `console` scope token (and the `viewer` role) now reads what 0.4 kept for admin:
    the settings (secrets show only whether they are set), diagnostics, the outbound
    decisions, the AI usage, per-key usage, saved tests, schedules and their runs,
    source control status and history, and the AI agents; it also previews and
    validates harness scenarios (they compute and send nothing), and the
    management server lists the matching tools;
  - any authenticated caller may read `GET /_rustybin/session`, sign out, and reach a
    self-service visit's own routes (which answer `404` with control auth on);
  - `DELETE /_rustybin/requests?session=...` needs `traffic.read` everywhere, as on an
    open control plane; clearing everyone's captures needs `traffic.clear`;
  - the `inspector` scope reads a mock's traffic (`/_rustybin/mocks/{name}/requests*`);
  - the `harness` scope lists the harness read tools on the management server;
  - a refusal says `reason: permission_required` and `permission` (it was
    `insufficient_scope` with the caller's `scopes`); an API key refused by a
    handler's check in shared mode gets `403` (was `401`), as the key layer already
    answered; `get_instance` answers `caller.roles` and `caller.permissions` instead of
    `caller.rights`; an agent's tool list answers each tool's `permission` instead of
    its `right`; `export_config` is a `mocks.read` tool (the whole instance still needs
    `config.export`).
- Users and roles on every surface, with invitations (access control, step 09;
  [Users and roles](users.md)). An admin invites people (`POST /_rustybin/users`
  answers a one-time link `/ui/#/join/<token>`, 72 hours, kept only as a SHA-256, at
  most 100 open), gives them roles, disables or deletes them; a reset is a new link.
  Nobody but its owner sees a password: the person sets it on the join page (`POST
  /_rustybin/join`, open and rate limited like the sign-in) and changes it under **My
  account** (`/_rustybin/me`, `PUT /_rustybin/me/password`). Custom roles under
  `/_rustybin/roles` (every mode). Guard rails: the last enabled admin stays, nobody
  changes their own roles, the first admin is never deleted, a role in use is never
  deleted, and nobody grants or manages more than they hold. The console's
  **Management, Users and roles** with the permission matrix, the account menu's **My
  account** and the join page; `rustybin users`, `roles`, `me` and `passwd`; the tools
  `list_users`, `invite_user`, `update_user`, `delete_user`, `list_roles`,
  `save_role`, `delete_role` (the admin's tool list is 132); `users:` and `roles:` in
  `rustybin.yaml` (never a password; a document from the repository changes display
  names and emails only).

  **Behaviour changes**: `PUT /_rustybin/password` changes the caller's own password
  (any signed-in user; a caller who is not a user still needs `users.manage` and
  changes the first admin's); a disabled user's sign-in answers as a wrong password; the `apply_config` tool now checks what
  a document's sections need (`settings:`, `secrets:`, `agents:`, `users:`, `roles:`)
  as the REST route did; `list_sessions` and `end_session` take `user`.
- Open and closed APIs on the dev portal (access control, step 13;
  [Dev portal](portal.md#open-and-closed-items)). Each published item is `open` (the
  default) or `closed` (`access` on `PUT /_rustybin/portal/items/{id}`, in `GET
  /_rustybin/portal`, `portal.items` of `rustybin.yaml`). A closed item is read by
  everyone (docs, operations, schemas, spec, reference); Try it, the demo
  credentials, the collections and the MCP connection snippets unlock for a reader:
  a user holding the new `portal.read` permission (every built-in role; the
  `portal_reader` role holds nothing else) signed in to the portal, or a bearer
  control credential that resolves it. The catalogue answers a closed item to anyone
  else with `locked: true` and placeholders for the credentials; its collections
  answer `401`. A mock closes only with its mocked security on (`409 security_off`
  otherwise; `secure: true`, the console's **Turn on security and close**, turns it
  on and deploys it), so a call without the credentials is refused by the mock. The
  portal sign-in: `POST /portal/api/login`, `/logout`, `GET /portal/api/session`,
  the `rustybin_portal` cookie (`HttpOnly`, `SameSite=Strict`, `Secure` behind HTTPS,
  `Path=/portal`, 12 hours, its own signing subkey), accepted under `/portal` only;
  the throttle is the console sign-in's. An invitation of a `portal_reader` links to
  `/portal/#/join/<token>`. The console's publish dialog (Open or Closed), the lock
  and the reader count in **Management, Portal**, **Invite a portal reader**; the
  portal's **Sign in**, lock, disabled Try it, **Sign out** and join page;
  `publish_portal_item` takes `access` and `secure`; `rustybin portal publish
  --access closed [--secure]`. A document from the repository may close an item,
  never open one.

  **Behaviour changes**: `portal.auth=password` now asks for the portal sign-in (the
  0.3 screen that kept a console token in the tab is gone; on an instance without the
  password sign-in the portal still takes a control token) and lets in only
  credentials that hold `portal.read`: a control JWT with the `inspector` or
  `harness` scope alone no longer reads such a portal; with it, the reference page
  needs the sign-in too. A console sign-in (and an invitation) of an account whose
  only permission is `portal.read` answers `portal_only` with the portal's cookie,
  and the console sends the person to the portal.
- AI agents act for a user (access control, step 12;
  [Agents act for a user](agent-mcp.md#agents-act-for-a-user)). An agent on a user's
  sign-in token, and one whose OAuth sign-in a user approved, works for that user and
  never holds more than them: its permissions are the user's at every call (a role
  change applies to the next call), within the scope it was given, then its ceiling,
  allow and deny lists, rules and defaults take away. The agents list answers each
  agent's `owner` (`null` for the admin token and control JWTs, which act for nobody)
  and `owner_label`, `?owner=<user>` keeps one user's; the console's **AI agents**
  view has an **Owner** column and filter; `list_agents` takes `owner`; `rustybin
  agents list --owner`. A **ceiling may name a role** (built-in or custom: the tools
  whose permission it grants); the four 0.3 ceilings keep exactly their tools (nearest
  roles: `read-only` the viewer, `traffic` the viewer plus `traffic.read`, `manage`
  the editor, `full` the user's own). The new setting `agents.default_ceiling_for`
  (`role=ceiling`, such as `admin=editor`) gives a role's users' agents a default.
  `rustybin.yaml`'s `agents:` rules match `user` and `role`, and `credential: user`.
  **The consent page** lists the permissions the agent will hold (`grants`,
  `permissions` and `approver` in `GET /_rustybin/oauth/requests/{id}`); a user grants
  a scope that gives at least one permission they hold. **Your own agents**: seeing
  everyone's needs `agents.read`, changing them `agents.manage`; without them a user
  sees, limits (never widens), revokes and ends the sessions of their own, and
  restores one they revoked. Disabling or deleting a user revokes every agent acting
  for them and ends their sessions. Agents recorded before the upgrade move to their
  user at the first start (a console record to its user, an OAuth record to the first
  admin).

  **Behaviour changes**: the first admin's password (or user name) change signs out
  the first admin only (their sessions, portal sessions and agents, OAuth tokens
  included), no longer every user; an OAuth agent approved with the admin token or a
  control JWT outlives it (`DELETE /_rustybin/sessions` ends it). OAuth tokens carry
  the user (`rb_uid`) and the user's generation (`rb_ugen`); a token issued before the
  upgrade counts as the first admin's (with the password sign-in). Any signed-in
  caller reaches the `/_rustybin/agents*` routes; a caller who is no user still needs
  `agents.read` and `agents.manage` there, so what it can do is unchanged. The agents'
  `credential.kind` is `user` (with `via`: `sign-in` or `oauth`) for a user's agent
  (it was `console`, or `oauth` for one approved in the console); `match.credential:
  console` and `oauth` still match them. Disabling or deleting a user answers
  `agents_revoked`.

- Single sign-on with OpenID Connect (access control, step 10;
  [Single sign-on](sso.md)): with the `sso.*` settings (`RUSTYBIN_SSO_*`, all
  `settings.security`) the sign-in screen offers "Sign in with" any OpenID Connect
  provider, and its groups decide the roles through a role map (the union of a
  person's groups' roles, else the default roles). The authorization code flow with
  PKCE, `state` and `nonce`; the ID token verified (issuer, audience, expiry, nonce;
  RS256 or ES256) with the provider's keys, fetched with discovery under the new
  outbound purpose `sso` (the issuer's host) and cached for an hour; the provider's
  tokens are never kept. Users from single sign-on (`source: sso`) are created at
  their first sign-in (`sso.provision`, or added ahead with `"sso": true`), have no
  password and no invitation, get their roles again at every sign-in, and may have
  roles pinned by an admin (`pinned_roles`). `sso.allowed_domains` limits the email
  domains; `sso.password_sign_in` off leaves only single sign-on and the admin token.
  New routes: `GET /_rustybin/sso/status`, `/start`, `/callback`, `POST
  /_rustybin/sso/token` (open, throttled like the sign-in) and `POST
  /_rustybin/sso/test`; console: the sign-in button, **Management, Sign-in, Single
  sign-on** with **Test**; CLI: `rustybin login --sso` (a loopback address takes the
  browser back), `rustybin sso status`, `rustybin sso test`, `users invite --sso`,
  `users update --pin-role`; management MCP: `test_sso` (133 tools for an admin);
  `rustybin.yaml`: the `sso.*` settings, `sso: true` on a user. The demo identity
  provider puts the groups in the ID token when the `groups` scope is asked for. A
  new diagnostics check, `sso`.

  **Behaviour changes**: a document pulled from git never changes an `sso.*`
  setting; `update_settings` asks to confirm a change of `sso.*`; `GET
  /_rustybin/session` answers `obtained: "sso"` for a single sign-on token;
  `PATCH /_rustybin/users/{id}` refuses `roles` for a single sign-on user (409
  `sso_user`) and takes `pinned_roles`; an invitation, a reset or a password change
  for one is refused (409 `sso_user`).
- Projects: roles on part of an instance (access control, step 11;
  [Projects](projects.md)): a project (at most 100) groups mocks, saved tests and
  checkpoints (a mock's portal item follows its mock), and a user's roles in a
  project apply to its resources on top of their instance roles (the union rule; a
  user with a role in a project also holds `instance.read`). Another project's
  resource answers `404` and lists leave it out; instance-wide things need instance
  roles; a move between projects needs the write permission in both; a project is
  deleted only when empty or with `move_to` (another project or `instance`); the
  last enabled admin is counted instance-wide. New routes `GET /_rustybin/projects`
  and `GET`, `PUT`, `DELETE /_rustybin/projects/{name}`; `project` on the mock,
  saved test and checkpoint routes that create, `?project=` on those that list,
  `projects` on invitations, the users' answers and the session. A project's
  checkpoint keeps and restores that project's mocks only. `sso.role_map` entries may
  name a project (`{group, project, roles}`, kept as `group=project:role`). Console:
  the **Project** picker in the top bar, a mock's **Project** (move it), **Management,
  Users and roles**: the **Projects** card and each user's project roles. CLI:
  `rustybin projects list | show | save | delete --move-to`, `mocks move`,
  `--project` on the commands that list and create, `users invite --project
  P=role`. Management MCP: `list_projects`, `save_project`, `delete_project` (136
  tools for an admin), `project` on the tools that list and create. `rustybin.yaml`:
  `projects:` with members by user name, `project:` on mocks and tests; a pulled
  document never changes members, prunes a project nor moves a resource. An agent
  acting for a user follows the user's project roles.

  **Behaviour changes**: none for a caller with instance roles only (resources
  without a project are the instance's, as before). `GET /_rustybin/tests/runs`
  answers `total` as the runs the caller may see; the management tools on mocks,
  saved tests and checkpoints are listed for a member of a project who holds their
  permission there.
- The audit log (access control, step 14; [The audit log](audit-log.md)): who did
  what, and when. Every mutating control route and every management tool that
  changes something writes one entry (a new route or tool without code), with every
  sign-in and failed sign-in (the user named only when one has that name),
  invitations, role and project changes, single sign-on with the groups that mapped,
  portal sign-ins and items opened or closed, agent revocations (whose agent),
  config applies, git pulls with the commit, and every `403 permission_required`
  (reads and the data plane's included). An entry: time, actor (a user, or the
  credential's kind; the agent), via, action, event, targets, result, request id and
  address; never a value, a body, a header, a password or a token. The last 10,000
  entries in memory and, with a state directory, `<state>/audit/` as JSON Lines, one
  file a UTC day, written off the request path, kept `audit.retention_days` (30, at
  most 365) and at most `audit.max_bytes` (256 MiB; `RUSTYBIN_AUDIT_RETENTION_DAYS`,
  `RUSTYBIN_AUDIT_MAX_BYTES`), both `settings.security` and refused from a pulled
  document. Unauthenticated entries (sign-ins) are capped at 600 a minute. New routes
  `GET /_rustybin/audit` (filters `actor`, `action`, `target`, `since`, `until`,
  `result`, a cursor) and `GET /_rustybin/audit/stream` (SSE), both `audit.read`
  (only the `admin` role has it). Console: **Management, Audit** (filters, the table
  with links, older pages, a live tail, the retention settings). CLI: `rustybin
  audit list | tail`. Management MCP: `list_audit` (137 tools for an admin), typed
  names under `untrusted_*`. `rustybin.yaml`: the `audit.*` settings; the log is
  never exported.

- Security pass (area A): sign-in, invitations, single sign-on, the model and its
  enforcement (step 14 of the
  [plan](https://github.com/coldbathcode/rustybin/blob/main/docs/plans/testing-2/master-plan.md);
  every finding and its test in
  [the step's build notes](https://github.com/coldbathcode/rustybin/blob/main/docs/plans/testing-2/step-14-audit-log.md)).

  **Behaviour changes**:
  - Single sign-on links a user added ahead only through the email the provider
    verified (`email_verified: true`), never by a matching name; give such users
    their email. A new user's name comes from the email only when it is verified.
    The refusals about a name (`name_taken`, `other_identity`, `not_provisioned`,
    and a user added ahead without the verified email) reach the browser as one
    reason, `not_linked`; the detail is logged.
  - The agent tokens of the built-in OAuth server acting for a single sign-on user
    (access and refresh) are refused twelve hours after the person last signed in
    through the provider, until they sign in again.
  - Under an `https` issuer the discovery document's endpoints must be `https`.
  - A full table of single sign-on starts drops the oldest of the busiest address
    instead of refusing (`busy`).
  - `rustybin login --sso` answers only the callback carrying its own `cli_state`
    and prints what comes back without control characters.
  - Every password sign-in runs one argon2id verify under the hash permits, a dummy
    one without a hash, so a name nobody has can answer `429 busy` like a real one.
  - The brute-force brake counts per user name (20 failures a minute from every
    address together, was 100 for every sign-in together); invitations and single
    sign-on have lanes of their own.
  - Listed sessions: at most 16 per user (a sign-in past them ends the user's oldest
    session, whose token is refused) and 4096 in all (was 256 in all).
  - A first admin's reset link is refused with `409 locked` before it is used while
    `RUSTYBIN_SETTINGS_LOCK` holds the sign-in (the link stays valid).
  - `update_settings` needs what `PATCH /_rustybin/settings` needs for each key, and
    confirms `proxy.*`, `assist.provider` and `assist.base_url` changes too.
  - The `git.*` settings need `git.write` on top of `settings.write`.
  - `sso.role_map` and `sso.default_roles` may name only roles whose permissions the
    setter holds (in a project for `project:role`), on every surface.
  - `DELETE /_rustybin/sessions/{id}` (and `end_session` with an id) needs every
    permission of the session's user; `DELETE /_rustybin/sessions` (and `end_session`
    with `all`) needs every permission.
  - `GET /_rustybin/config/export` and `export_config` leave out `users:`, `roles:`
    and `projects:` for a caller without `users.read` (the `editor` role).
  - On a self-service instance a user or a control JWT is held to each route's
    permission (only a visitor, an API key or a caller sending a manage token gets
    the visitor rules), and a visitor reads the instance's mocks but needs the
    route's permission for their traffic, data, deletion or publication.
  - A document from the repository gives no role a permission it did not grant and
    creates no role that single sign-on names.
  - A `/_rustybin/*` route the catalogue does not know is refused (`403`).

- Security pass (area B: the portal's closed items, projects and agents; step 14 of
  the [plan](https://github.com/coldbathcode/rustybin/blob/main/docs/plans/testing-2/step-14-audit-log.md#security-pass-build-notes-area-b)).
  **Behaviour changes**:
  - a mock closes on the portal only when every call needs an API key or a Basic user
    of its own: closing is refused (`409`, `reason` `default_credentials`,
    `obtainable_credentials` or `open_operations` beside `security_off`, with
    `closable_modes`) for the documented defaults (`demo-key`, `demo` / `demo`), the
    `jwt` and `bearer` modes and OAuth 2, OpenID Connect, bearer and `mutualTLS`
    schemes (anyone mints a JWT from the built-in identity provider, whose demo client
    is public, or downloads the demo client certificate), and a mock whose every
    operation is public (operations declared public, marked so or without a
    requirement in `spec` mode, stay open by design);
  - `secure: true` ("Turn on security and close") replaces every default credential
    with a random one and uses `api_key` mode unless the spec's mode (or `basic`)
    closes the mock; metadata and capacity are checked before it changes the mock;
  - while an item is closed, no edit, deploy, rollback, document (trusted or pulled),
    file reload or checkpoint restore turns the mock's security off or puts the
    defaults back (`409`, a problem line); a mock that has such a revision anyway
    (recreated under the same name, loaded from a file, closed before this release
    behind `demo-key`) answers every call `403 closed_unsecured` until it is secured
    (send `secure: true` again), and a config file load reports it;
  - a document pulled from git never changes `portal.auth` nor `portal.base_url`;
  - a closed `mcp:*` (and every other non-mock) item drops the hint at how to get
    through, the publish answer has a `note` that its endpoint stays callable, and a
    locked mock entry carries no `token_url`;
  - portal sign-outs have their own bounded list, so they never push out console
    sign-outs;
  - a second revoke of an AI agent keeps the first revocation (who, when); the answer
    says `already_revoked`;
  - revoking a user's OAuth agent refuses that user's tokens of the client only, never
    another user's who registered the same client metadata (user disabled or deleted
    too);
  - the AI spec job lists only the mock names the caller may read as taken;
  - a mock's live traffic stream checks its caller again at every event and every 5
    seconds, and ends when they may no longer read it;
  - the OAuth consent shows (`projects`, per scope and for the request) and counts the
    approver's project grants;
  - naming an unknown project answers like a project the caller may not use (`403`)
    unless they hold the permission on the whole instance, and a checkpoint restore
    says "now in another project";
  - `GET /_rustybin/projects` lists a project's resource names only to who reads them
    there, and `GET /_rustybin/mocks` answers `load_errors` only to holders of
    instance-wide `mocks.write`.

## 0.4.0 (2026-11-03): testing workflows

Saved tests run without anyone at the console and keep their history, four more
gateway scenarios, checkpoints that travel in the configuration repository, and a
compare view when an edit is refused as stale
([the plan](https://github.com/coldbathcode/rustybin/blob/main/docs/plans/testing-2/master-plan.md),
steps 01 to 06; access control follows in 0.5.0).

**Behaviour changes** (what an operator or a script may notice):

- A saved test can no longer be named `runs` or `schedules` (the new routes take those
  paths); an existing test with either name is skipped with a warning when the instance
  loads.
- `POST /_rustybin/tests/run` answers more fields (`run_id`, `trigger`, `principal`,
  `selection`, `change`); with `?format=junit` the run id is in the
  `X-Rustybin-Test-Run` header. Every value given as a variable is replaced by its
  `{{NAME}}` in the answer and in the kept run.
- Every gateway scenario lists its test `parameters`; a check's body is the gateway URL
  plus a map of parameters, and an unknown or out-of-range parameter is refused with a
  `400` naming the bounds before anything is sent.
- The mock store keeps the drafts replaced before they were deployed (as many as
  deployed revisions, in memory), so a stale edit's `409` names what changed for an
  undeployed base too.
- A commit through source control never stages `<git.path>/checkpoints/` except the
  checkpoints it names, and "nothing to commit" ignores untracked files.
- `rustybin.yaml` has a `test_schedules:` section (empty when there are none).
- The management MCP server has 125 tools for an admin; full rights without the
  harness tools no longer fit one `tools/list` page (see
  [tool limits](coding-agents.md#tool-limits)).

**New**:

- **A history of saved-test runs**: every run of the [saved tests](saved-tests.md#history)
  (console, CLI, API, management MCP) is kept with its trigger, the caller (never a
  credential), each test's result and what changed since the previous run (newly
  failing, newly passing); the last 200 for 30 days in `<state>/test-runs/` (50 in
  memory without a state directory), never a variable's value. `POST
  /_rustybin/tests/run` answers `run_id` and `change`; `GET`, `DELETE
  /_rustybin/tests/runs`, `GET /_rustybin/tests/runs/{id}` (`?format=junit`);
  `rustybin tests runs | run-show | runs-clear`; the tools `list_test_runs`,
  `get_test_run`, `clear_test_runs`; the console's Saved tests view has a History
  card. A test can no longer be named `runs`.
- **Saved tests on a schedule, with a failure notice**: a [schedule](saved-tests.md#schedules)
  runs saved tests (every one, `names` or a `prefix`) `every` 5m to 24h or `at` a time of
  day (UTC), without anyone at the console; each run is in the history as
  `schedule:<name>`. A run never overlaps another run of the saved tests, and one due
  while the harness sends load is skipped and counted. A failing run posts one JSON
  notice to the schedule's `webhook` (and a recovery one more with `notify_recovery`)
  under the new egress purpose `notify`, which has no default; a refusal is reported in
  the status and never retried, a failed delivery is tried 4 times. The secret
  `notify.webhook_token` (`RUSTYBIN_NOTIFY_WEBHOOK_TOKEN`) is its bearer token,
  `notify.base_url` (`RUSTYBIN_NOTIFY_BASE_URL`) makes its link absolute. At most 20
  schedules. `GET`, `POST /_rustybin/tests/schedules`, `GET`, `PUT`, `DELETE
  /_rustybin/tests/schedules/{name}`, `POST /_rustybin/tests/schedules/{name}/run`;
  `rustybin tests schedules | schedule-show | schedule-save | schedule-run |
  schedule-delete`; the tools `list_test_schedules`, `save_test_schedule`,
  `delete_test_schedule`, `run_test_schedule`; a Schedules card on the console's Saved
  tests view; `test_schedules:` in `rustybin.yaml` (a pulled document cannot name a new
  webhook). A test can no longer be named `schedules`.
- **Two more [gateway scenarios](gateway-scenarios.md)**: `circuit_breaker` (the
  gateway stops forwarding to a failing upstream, then lets a trial through after the
  half-open time you give as `half_open_ms`) and `request_size_limit` (a body over the
  `size_limit` you give is answered `413` and never reaches the upstream). Every
  scenario lists its test parameters with their bounds under `parameters`; the console
  shows a field for each, the CLI adds `--half-open-ms` and `--size-limit`.
- **CORS and header transformation [gateway scenarios](gateway-scenarios.md)**: `cors`
  (preflights from the `allowed_origin` and `refused_origin` you give, then a `GET`: the
  allowed preflight is answered with CORS by the gateway, or by the upstream with
  `upstream_cors: true`, the refused one gets none) and `header_transformation` (the
  request header the gateway adds reaches the upstream, the one it removes does not,
  the response header it adds is on the answer). Test parameters now also take origins,
  header names and booleans; the CLI adds a flag for each (`--allowed-origin`,
  `--upstream-cors`, `--add-request-header` and the rest), and an unknown parameter is
  refused with the list. A `cors` run's mock gets no CORS from the instance itself.
- **Checkpoints in the configuration repository**: `POST /_rustybin/git/commit` takes
  `checkpoints` and writes those [checkpoints](checkpoints.md#in-the-configuration-repository)
  to `<git.path>/checkpoints/<name>.json` next to `rustybin.yaml` (only a commit that
  names one: the automatic mode never writes one, and no commit stages anything else in
  that directory). A pull answers the repository's checkpoints by name; `GET
  /_rustybin/checkpoints?from=git` lists them with what each holds, and `POST
  /_rustybin/checkpoints/{name}/restore?from=git` (with `dry_run`) restores one. A
  checkpoint from the repository is untrusted: size-checked before it is read (16 MiB),
  parsed and held to a local checkpoint's caps, compiled on the compile pool.
  `rustybin git commit --checkpoint NAME`, `rustybin checkpoint list --from-git`,
  `rustybin checkpoint restore NAME --from-git`; `git_commit` gains `checkpoints`,
  `restore_checkpoint` gains `from`, the new tool `list_repository_checkpoints`; the
  console's Checkpoints view gets an "In the repository" card and the Source control
  commit card a checkpoint choice.
- **A compare view for a stale edit**: an edit the console makes from a revision someone
  changed since (the `409 rustybin_stale_revision`) opens a
  [compare dialog](mocks/index.md#comparing-a-refused-edit): per section (spec,
  responses, security, MCP, each operation) what the other change did beside the
  refused edit; Keep mine sends it again on the current revision, Take theirs reloads.
  The new `GET /_rustybin/mocks/{name}/revisions/{revision}` (the `get_mock_revision`
  tool, `rustybin mocks revision NAME N`) answers a kept revision's spec and settings
  and what the draft changed since; the store now also keeps the drafts replaced before
  they were deployed (as many as deployed revisions), so the `409`'s `changed` names
  the sections for an undeployed base too.

**Also**:

- **Every tool on one page**: `?page_size=200` on the management MCP URL (it was
  accepted before) now also works with OAuth sign-in, whose resource check ignores the
  query. The new [Connect your coding agent](coding-agents.md) page uses it for the
  clients that read only the first `tools/list` page (Cursor, Gemini CLI, Zed).
- `rustybin mock push | list | ...` sends `RUSTYBIN_TOKEN` and `RUSTYBIN_ADMIN_TOKEN`
  to a `--server` only when it is `RUSTYBIN_SERVER` or loopback, follows no redirects
  and warns on plain http, as the other commands do since 0.3.1.
- `POST /_rustybin/mocks/apply` compiles on the compile pool.
- The `rustybin.yaml` schema allows response statuses from 200, as the validation does.
- Release binaries for Linux and macOS on every GitHub release; the
  [CLI guide](cli-guide.md#install) says how to install. The docs site has its
  favicon, and page-view analytics when the site is configured for them.

`cargo` version 0.4.0, the tag `v0.4.0`, the signed multi-arch image
`ghcr.io/coldbathcode/rustybin:0.4.0`, digest
`sha256:db4d1493d6121556d456463a5027530cc91e1a30c351427d8e182ecefeb1ea5e` (verify it with the
cosign command of the [quick start](quick-start.md#docker); the
[release](https://github.com/coldbathcode/rustybin/releases/tag/v0.4.0) carries the same
values, and the Linux and macOS binaries).

## 0.3.1 (2026-11-02): the 0.3 review

A full review of 0.3.0 by eight reviewers, 89 findings, all fixed in eight steps
([the plan](https://github.com/coldbathcode/rustybin/blob/main/docs/plans/review-0.3/master-plan.md));
the [threat model](concepts/public-instances.md) says what each kind of caller can do now.

**Behaviour changes** (what an operator or a script may notice):

- **Sign in again after the upgrade.** The console and OAuth signing key is a random
  per-instance key in `<state>/control-oauth.key` (per process without a state
  directory) unless `RUSTYBIN_CONTROL_OAUTH_KEY` is set; it is never derived from the
  admin token. Sign-in and OAuth tokens issued before the upgrade stop working once
  (agents too). Replicas that shared sessions through a shared admin token set
  `RUSTYBIN_CONTROL_OAUTH_KEY` or share the state directory. An admin token shorter than
  24 characters warns at startup.
- **Control-plane CORS.** `/_rustybin/*` (the sign-in and OAuth endpoints included)
  answers cross-origin only to origins named in `RUSTYBIN_CORS_ORIGINS`; `*` covers the
  data plane only. A browser app on another origin that calls the control API lists its
  origin.
- **The sign-in changes only with the password.** In password mode `console.user` and
  `console.password_hash` change only with the current password (`PUT
  /_rustybin/password`, or `current_password` in `PATCH /_rustybin/settings`) or the
  admin token itself; the management MCP and `rustybin.yaml` no longer change them, and
  every change signs every session out.
- **Saved tests are admin-only to read** (`GET /_rustybin/tests*`, `list_tests`), a
  secret-looking header or query value must be a `{{variable}}` (a literal is refused),
  captured JSON and form bodies lose their secret fields to variables, and a binary
  capture cannot be saved. One run at a time (`409`); `run_tests` asks before replaying
  a `POST`, `PUT`, `PATCH` or `DELETE`. A save or delete the file refuses answers `500`
  and changes nothing.
- **A prune deletes only the instance's own mocks**: never a visitor's, an API key's or
  a running gateway scenario's, and never a mock its document names but could not read
  (a pulled entry with a typo included). The harness and registry sections prune only
  what the document does not name at all.
- **A document from git cannot repoint outbound requests**: `egress.allow`,
  `egress.allow_loopback`, `assist.provider`, `assist.base_url` and
  `proxy.client_ip_header` with a different value are reported and kept, a test cannot
  name a new gateway, and `secret:stored` in a harness scenario fills a saved value only
  for the same targets. A pull's preview and apply read the same commit.
- **Compiles count for every caller without admin rights** (30 a minute per visit, key
  or address, whatever the session header), duplicates, AI previews and generated
  examples included; config applies run on the compile pool (`503` with `Retry-After`
  when it stays busy).
- **Deploys can name their revision**: `POST /_rustybin/mocks/{name}/deploy` takes
  `If-Match` or `expected_revision` (`409` when the draft moved on;
  `deploy_mock.expected_revision`, `rustybin mocks deploy --expect-revision`), and
  `PATCH` with `deploy: true` publishes exactly the revision it wrote. Revision numbers
  start from the clock, so they keep growing across restarts (they are large numbers).
- **Agents see less third-party text in summaries**: traffic rows carry
  `untrusted_path`, `untrusted_query`, `untrusted_method` and `untrusted_validation`,
  `diagnose_instance` moves quoted messages to `untrusted_message`, and
  `list_mcp_check_runs` answers `untrusted_report`. MCP inspector targets never show
  their query string.
- **Without admin rights the MCP relay, check runs and `mcp_call` send only read-only
  methods** to servers on other hosts (no `tools/call`). The inspector runs at most two
  check suites and 16 relay calls at once (`503`).
- **Agents cannot lift their own limits** (rights, restore, end sessions of their own
  credential); `update_settings` asks to confirm changes to `egress.*`, `agents.*`,
  `console.*` and `git.*`; AI assist caps of token callers follow the credential, not
  `X-Rustybin-Session`.
- **Statuses are 200 to 599**: a requested (`Prefer: code=`, `X-Rustybin-Mock-Status`) or
  configured mock status outside that range is refused.
- **Public mode**: `Set-Cookie` through `/response-headers` and a cookie `_domain` are
  refused (`400`); at most 20 bins per address; public A2A sink ids are at least 16
  characters and keep 10 notifications; dynamic client registrations, registry token
  mints and portal catalogue renders are rate limited (`429` or `503` with
  `Retry-After`); a full token denylist refuses new revocations (`503`) rather than
  forgetting old ones.
- **The CLI** follows no redirects, sends `RUSTYBIN_TOKEN` and `RUSTYBIN_ADMIN_TOKEN` to
  `--url` only when it is `RUSTYBIN_URL` or loopback, warns on plain http to another
  host, and reads a secret test variable with `--var NAME=-` (stdin) or `NAME=@file`.
- **Gateway scenario run ids are 96 bits**, a check cannot be skewed by forged sessions,
  and the credential-injection key is shown to admin callers only (`key_hidden`).

**Hardening** (no change for a well-behaved caller):

- Listeners: a request head must arrive within 30 seconds, at most 16,384 connections at
  once; WebSocket sends and gRPC streams are bounded in time.
- Budgets: schema generation (`minLength`, exclusive bounds, unique items), `$ref`
  inlining (20,000 nodes), `allOf` walks, the regex cache (128 patterns, 256 KiB), rerank
  queries, completion stream pacing, A2A message and task bytes, captures (16 KiB of
  headers, 4 KiB a field, a 64 MiB ring).
- Locks and files: checkpoint restores one at a time under the edit lock, each mock
  swapped in whole; mock, visit and saved-test files written off the async workers with
  the latest change winning; `apply_keeping` one locked edit per mock; session saves
  under one writer lock.
- Git: blobs are sized before they are read; an ssh user name cannot be an option.
- Harness: a member cannot depose the leader, a kill during preparation prevents the
  start, node calls never reach forbidden addresses, run ids are safe file names, one
  in-flight slot per request.
- `168.63.129.16` is never an egress target; logs escape control characters in user
  names and targets; in-process tool requests carry the caller's address, not
  `127.0.0.1`; cookie arguments of a mock's MCP tools are percent-encoded.
- Console: the console token never fills the MCP inspector's auth form, edits and
  deploys name the revision they were made from, OAuth popups have no opener, sign-out
  clears the tab's data, consent redirects refuse script schemes.
- The image workflow keeps `latest` on main's newest build.

`cargo` version 0.3.1, the tag `v0.3.1`, the signed multi-arch image
`ghcr.io/coldbathcode/rustybin:0.3.1`, digest
`sha256:079a0a8a6808db5cdc72e43c59c5f3d7bae7ebe1ae26823d2036209427bd6601` (verify it with the
cosign command of the [quick start](quick-start.md#docker); the
[release](https://github.com/coldbathcode/rustybin/releases/tag/v0.3.1) carries the same
values, and the Linux and macOS binaries).

## 0.3.0 (2026-11-02): reliability and testing workflows

The correctness work the hardening review queued, then the workflows that turn an
instance into a test bench for a gateway, in ten steps
([the plan](https://github.com/coldbathcode/rustybin/blob/main/docs/plans/reliability/master-plan.md)).
Every capability is on the API, the console, the CLI, the management MCP and
`rustybin.yaml` (or says why not).

**New**

- **[Diagnostics](troubleshooting.md#ask-the-instance-first)**: `GET /_rustybin/diagnostics`,
  `rustybin doctor` (exit `1` on a failed check), `diagnose_instance`, Management,
  Diagnostics. Each check (the state directory, the secrets key, unreadable secrets, the
  settings document, load errors, the config file, listeners, outbound refusals, source
  control, the default password, the harness engine, AI assist) says what it saw and the fix.
- **[Gateway scenarios](gateway-scenarios.md)**: retries, rate limiting, credential
  injection, timeouts and caching. A run prepares the upstream, gives the steps for the
  gateway, sends test traffic through it (egress purpose `scenario`) and compares what the
  client got with what reached the upstream; ending it removes only what it created.
- **[Saved tests](saved-tests.md)**: a captured request (**Save as test** in Live traffic)
  or a written one with assertions (status, a header, the body, a JSON value, the time),
  run in process or through a gateway (egress purpose `replay`), as JSON or JUnit, and
  exported as Hurl for CI. Credentials become `{{variables}}`, never values. In
  `rustybin.yaml` as `tests:`.
- **[Checkpoints](checkpoints.md)**: save the instance's API mocks (with their data) and
  the health toggle, restore them before a run (with a dry run), never touching visitors'
  mocks, settings or secrets.
- **[Edit conflicts](mocks/index.md#two-people-editing-one-mock)**: a mock edit may name the
  draft revision it was made from (`If-Match` or `expected_revision`, the `ETag` of the mock);
  a stale one answers `409` with what changed. The console, the CLI and agents always send
  one; `mocks.require_revision` makes it mandatory (`428`).
- The AI job fields offer the provider's models to pick from.

**Behaviour changes** (what an operator or a script may notice):

- **An apply never deletes what its document names but cannot load.** A mock whose spec
  fails to read, parse or compile is a problem and keeps its last good revision, in the
  apply and in its dry run alike (it was pruned before).
- **A dry run validates like the apply**: settings, secrets, mocks (compiled), portal
  items and harness scenarios a dry run accepts are the ones the apply accepts.
- **Git documents and specs are read whole**: a spec over 8 KiB pulled from a repository
  was cut before; specs now go up to the instance's spec limit, and a larger one fails the
  pull with a message.
- **Concurrent edits of one mock are no longer lost**: an edit without a revision that
  raced another is made again on top of it.
- **`tools/list` of the management server pages at 100 tools**, and the server now has
  more: clients follow `nextCursor` (MCP clients do). New tools come after the existing ones.
- The `rustybin.yaml` export has a `tests:` section (empty when there are none).

`cargo` version 0.3.0, the tag `v0.3.0`, the signed multi-arch image
`ghcr.io/coldbathcode/rustybin:0.3.0`, digest
`sha256:471f176542e10e4012b7f05f972985704f22e927bef287ceec97136bb2c5ac6f` (verify it with the
cosign command of the [quick start](quick-start.md#docker); the
[release](https://github.com/coldbathcode/rustybin/releases/tag/v0.3.0) carries the same
values).

## 0.2.2 (2026-11-02): the hardening release

No new features. A full code review of 0.2.1 found what a public instance exposes; every
finding is fixed, in ten steps ([the plan](https://github.com/coldbathcode/rustybin/blob/main/docs/plans/hardening/master-plan.md)).
The [threat model](concepts/public-instances.md) says what each kind of caller can and
cannot do now.

**Behaviour changes** (what an operator or a script may notice):

- **The console sends its control token only on its own control calls.** It is opt-in
  per call, never inferred from a URL: a URL from an MCP server, a discovery document or
  an AI answer does not receive it. An AI `run_request` action is refused for
  `/_rustybin`, `/ui` and `/portal`.
- **A saved MCP connection's credentials go only to its own target**, and using a
  connection that holds credentials needs admin rights (`400`
  `connection_target_mismatch`, `403` `connection_needs_admin`). The inspector, the relay
  and the checks no longer target the management server (`/_rustybin/mcp`); the console
  still connects to it from the browser.
- **An AI agent's revocation and the operator's limits follow its credential**, not the
  client name it sends: another name on the same token is refused or limited too.
  `list_registry_servers` answers its entries under `untrusted_entries`, paged.
- **Sign-outs survive a restart.** With a state directory the token generation and every
  revocation live in `<state>/sessions.json`; without one a restart signs every token
  out. A sign out everywhere or a password change also ends the management server's
  OAuth tokens.
- **The proxy header trusted for the client address is a setting**,
  `proxy.client_ip_header` (`RUSTYBIN_TRUST_FORWARD_HEADER`): name your proxy's header;
  `any` keeps the old order. An `ip:` value of `X-Rustybin-Session` is ignored. A control
  JWT without the admin scope is never admin, also without an admin token.
- **Request bins answer inert content**: HTML, scripts and SVG as `text/plain`, no
  cookies or security headers of their own, a sandbox CSP and `nosniff`.
- **A document pulled from git is not expanded from the environment.** A secret takes
  only `${VAR}` naming its own variable, `secret:stored` or `null` (a literal value is
  refused); a mock's `spec:` file must be inside the repository; diffs never show secret
  values; pruning never removes the console's password hash. `RUSTYBIN_CONFIG_FILE`
  keeps expanding as before.
- **Saved harness scenarios** export their API keys and OAuth client secrets as
  `secret:stored`, answer them redacted, and are sealed at rest.
- **The demo identity provider on a public instance** sends demo clients on only to
  loopback or its own host (a page with the link otherwise), limits failed sign-ins per
  address, and exchanges only its own RS256 tokens (the HS256 sample is refused).
- **Outbound connections never use a system proxy** (`HTTP_PROXY` and friends are
  ignored), and a `*.domain` allowlist entry no longer reaches private addresses
  without a network entry.
- **New limits**: spec compiles four at a time (`503` when busy, 30 a minute for a
  visitor), generated values at 1 MiB, path templates at 32 parameters, server URLs at
  2 KiB, gRPC messages and client streams at `RUSTYBIN_BODY_LIMIT`, sign-in hashes four
  at a time, OAuth sign-in requests 10 a minute per address. `ai` is a reserved mock
  name. `mcp save-connection --secret` takes `name=-` or `name=@file`, never a value.
- **`assist.provider`** accepts `claude`, `google` and `openai-compatible` again.

**Fixes and additions by step:**

- **AI model catalogue** (step 01): `GET`, `POST /_rustybin/assist/models` and a model
  per AI job (`assist.job.<task>`) on every surface, with a Models and jobs card in the
  console.
- **Hostile specs and queries** (step 02): no single request can abort the process or
  hold a worker (see the limits above); the regex cache is an LRU with each revision's
  patterns pinned.
- **MCP** (step 03), **the console** (step 04), **sign-in** (step 05): as above; the API
  reference pages pin their script with Subresource Integrity and run sandboxed.
- **Egress and git** (step 06): every client resolves the pinned name only, so a name
  in another form cannot re-resolve (a DNS rebinding path the review confirmed); NAT64,
  6to4 and IPv4-compatible addresses are checked as their IPv4 destination; git is
  pinned, follows no redirect, runs with a cleared environment.
- **Configuration and persistence** (step 07): symbolic links in the working copy are
  refused; mock saves are ordered and written privately.
- **Public data plane** (step 08) and **the harness** (step 09): one run at a time under
  parallel starts and a kill during verification, elections only from eligible nodes,
  rate caps that hold under live multipliers, no engine panic on absurd answers.
- **Dependencies** (step 10): `tokio`, `anyhow` and `rand` patched; `serde_yaml`
  replaced by its maintained fork (`serde_norway`, the output pinned byte for byte);
  `rustls-pemfile` dropped; `tonic` 0.13 and `axum-server` 0.8, so one `axum`.

`cargo` version 0.2.2, the tag `v0.2.2`, the signed multi-arch image
`ghcr.io/coldbathcode/rustybin:0.2.2`, digest
`sha256:1fe578b7f45b34f61e9e4bd3944abe7b399001a42ac6f5ae5f0cd1681dddc9ad` (verify it with the
cosign command of the [quick start](quick-start.md#docker); the
[release](https://github.com/coldbathcode/rustybin/releases/tag/v0.2.2) carries the same
values).

## 0.2.1 (2026-10-30): the dev portal in the container image

- **Fix: the published 0.2.0 image served no dev portal.** `/portal` answered "portal
  assets are not embedded in this build": the Dockerfile copied `ui/` into the build
  context but not `ui-portal/`, so the portal SPA was embedded empty (a `cargo build`
  from a checkout was fine). The Dockerfile copies `ui-portal/` now, and the image smoke
  tests of CI and the publish workflow start the container with `RUSTYBIN_PORTAL=on` and
  require `/portal/` to answer the page.
- **Fix: OpenAI's reasoning models answered empty or cut off.** GPT-5 and the o-series
  spend output tokens thinking before they write, and the key test gave them 16; the
  describe task could end mid-JSON and report a schema problem. Requests to those models
  now carry `reasoning_effort: low`, a stream that ends with `finish_reason: length` is
  reported as "the model hit its output token limit" instead of a truncated answer, and
  the key test allows 256 output tokens.

## 0.2.0 (2026-10-30): the AI-assisted mocking service

Rustybin becomes a mocking and testing service a team runs: set up by describing what
you need, explored by the people who test against it, managed from the console, a CLI,
an AI agent or a `rustybin.yaml` in git, and locked with a password on day one. Built
in twelve steps ([the plan](https://github.com/coldbathcode/rustybin/blob/main/docs/plans/next/master-plan.md)), each listed below.

**Changed default: the console and the control plane now ask for a user name and
password.** `RUSTYBIN_CONTROL_AUTH` is `password` when unset: `rustybin` / `rustybin`
until changed (`RUSTYBIN_CONSOLE_PASSWORD`, or Management, Sign-in). Instances that
relied on an open control plane set `RUSTYBIN_CONTROL_AUTH=open`; `token` and `jwt`
are unchanged; a public instance refuses to start with the default password. Every
other change is additive: a 0.1 `rustybin.yaml` still loads, every 0.1 route and
variable keeps its meaning.

- **Your own OIDC users.** `RUSTYBIN_OIDC_USERS_FILE` replaces the built-in provider's demo users
  with users of your own (PBKDF2-HMAC-SHA256 password hashes from the new
  `rustybin oidc-hash-password`), so the provider can guard something real; the login page stops
  listing the demo passwords. An invalid file stops the server.
- **Foundations** (step 01). One state directory, `RUSTYBIN_STATE_DIR`: settings,
  the secrets key, and the defaults of the mocks and harness state directories.
  Instance settings at `GET`, `PATCH /_rustybin/settings` and `DELETE
  /_rustybin/settings/{key}` (admin): dotted keys with a schema, seeded by their
  environment variables and overridden by what is saved; `RUSTYBIN_SETTINGS_LOCK`
  makes the environment authoritative. Secrets sealed at rest with
  ChaCha20-Poly1305 (`RUSTYBIN_SECRETS_KEY` or a generated `secrets.key`), returned
  only as `{set, hint}`, audited without values. One egress policy for every
  outbound call (`src/egress.rs`): `RUSTYBIN_EGRESS_ALLOW` and
  `RUSTYBIN_EGRESS_ALLOW_LOOPBACK` plus a rule per feature, pinned addresses,
  always-refused ranges, `GET /_rustybin/egress` with the last 100 decisions; A2A
  push notifications, the leads webhook and the key registry URL now resolve
  through it (their variables and semantics are unchanged, except that
  `RUSTYBIN_A2A_PUSH_ALLOW_ALL` no longer reaches link-local or cloud metadata
  addresses). The console title and back link are the settings `console.title` and
  `console.backlink`; the leads webhook token is the secret `leads.webhook_token`.
  The console's settings dialog is replaced by the **Management** view (the gear
  opens it): General (console settings, this browser, the agent panel) and Network
  (the policy, each feature's rule, the decisions, the leads token), with forms
  generated from the settings schema.
- **Console sign-in with a user name and password** (step 02). **Changed default:**
  `RUSTYBIN_CONTROL_AUTH` is `password` when unset (`jwt` in hosted mode, `open` in
  shared mode): the control plane and the console ask for `rustybin` / `rustybin`
  (`RUSTYBIN_CONSOLE_USER`, `RUSTYBIN_CONSOLE_PASSWORD` or an argon2id
  `RUSTYBIN_CONSOLE_PASSWORD_HASH`). Instances that relied on an open control plane
  set `RUSTYBIN_CONTROL_AUTH=open` (self-service instances must); `token` and `jwt`
  are unchanged. `POST /_rustybin/login` (never behind control auth) answers a 12
  hour control token (HS256, the instance key, scope `admin`) that every
  control-plane route, the management MCP server and the admin guard accept;
  `POST /_rustybin/logout` revokes it; `GET /_rustybin/session` says who a credential
  is; `GET`, `DELETE /_rustybin/sessions`, `DELETE /_rustybin/sessions/{id}` and
  `PUT /_rustybin/password` (admin, the current password required) manage sessions
  and the password, which is stored as the secret `console.password_hash` (the user
  name as `console.user`). Five failed attempts per address and minute, then `429`
  with `Retry-After`, doubling up to ten minutes; attempts are audited on
  `rustybin::auth` without the password. With the default password the instance
  warns at startup, `/_rustybin/status` carries `default_password: true` and the
  console shows a banner; a public instance refuses to start. The console: a user
  name and password sign-in screen with Remember me (the token form stays under "Use
  a token instead"), the signed-in user and expiry in the top bar, and Management,
  Sign-in (change the user name and password, the sessions, Sign out everywhere).
  The docker image's CI smoke test signs in with the defaults.
- **AI assist: providers, keys, the assist bar** (step 03). The console's AI features
  run on the user's own provider and key: `RUSTYBIN_ASSIST_PROVIDER` (`anthropic`,
  `openai`, `gemini`, `compatible`, `mock`), `_MODEL`, `_FAST_MODEL`, `_BASE_URL` and
  one `_*_KEY` per provider seed the `assist.*` settings (keys are secrets: sealed,
  `{set, hint}`, never in prompts, exports or captures). Every call is a task with a
  validated input, a system and user prompt, an optional output schema (the provider's
  native JSON mode, validated with the mocks' validator, repaired at most twice) and a
  deterministic mock answer: `free_answer`, `explain`, `suggest` this step. Routes
  `GET /_rustybin/assist`, `POST /_rustybin/assist/run` (SSE events `sent`, `delta`,
  `json`, `action`, `usage`, `error`, `done`, or one JSON document),
  `POST /_rustybin/assist/test` and `GET /_rustybin/assist/usage` (admin). Egress under
  the `assist` purpose (the provider's host once a key is set); caps
  `RUSTYBIN_ASSIST_MAX_CALLS_PER_MINUTE` (20, per caller, `429`) and
  `RUSTYBIN_ASSIST_MAX_OUTPUT_TOKENS` (8192); a redact pass refuses inputs that hold a
  stored secret or the admin token (`422`); usage per provider, model and task and the
  metrics `rustybin_assist_calls_total`, `rustybin_assist_tokens_total`. The console:
  the assist bar (`Ctrl+K`) with per-view suggestions, the drawer with streamed
  markdown, "What was sent", tokens and estimated cost, apply cards (`open_view`,
  `copy`, `run_request`), sparkles in the navigation, an **Ask Rustybin** card on the
  overview, **Explain** on a captured request, and Management, AI (curated models with
  prices, keys, Test the key, the built-in mock LLM, usage). New book page
  [Bring your own AI](ai-assist.md).
- **AI mocks: specs from a prompt, examples, AI edit** (step 04). Four tasks:
  `spec_from_description` (an OpenAPI 3.1.0, 3.0.3 or Swagger 2.0 document from a
  description with auth, pagination and error style, parsed and run through the
  deploy checks), `examples_for_operations` (example payloads for the success
  responses that lack them, each validated against its schema, ten operations per
  call), `edit_spec` (the whole document after an instruction, with a bounded
  structural diff) and `request_examples`. Examples stored with a mock
  (`operations.<id>.examples.<status>`, source `user`, `ai` or `generated`, at most 8
  per status) are served where the spec has none and by name, by the REST routes and
  the mock's MCP tools alike. Every revision reports its **example coverage**: a
  banner on the detail page with **Generate with AI** and **Use generated values**
  (the deterministic generator, no key), a `no_examples` warning in the deploy
  report, `check_mock` and `rustybin mock validate`, the count in `get_mock`, and
  `GET /_rustybin/mocks/{name}/coverage`. Routes `POST /_rustybin/mocks/ai/spec`,
  `POST /_rustybin/mocks/{name}/ai/examples` and `POST /_rustybin/mocks/{name}/ai/edit`
  (SSE progress or one JSON document); visitors need `RUSTYBIN_ASSIST_VISITORS`.
  Console: the wizard's **Describe it** tab (first when a key is set), the coverage
  banner, the Generate examples dialog with a preview per operation, **Edit with AI**
  with a streamed diff and Apply as draft, stored examples in the operation editor,
  and the apply cards `create_mock`, `edit_spec`, `generate_examples` from the assist
  bar. New book page [Build mocks with AI](mocks/ai.md).
- **MCP inspector for any server, every auth mode, test runs** (step 05). The console's
  MCP view is rebuilt in three columns (the server and connection, the protocol tabs,
  the timeline with exports) and connects to any MCP server: this instance's servers
  from the new `GET /_rustybin/mcp-servers` (`/mcp` and its variants, the management
  server, every mock's server; a mock's MCP tab opens the inspector on it), saved
  connections, or any URL; over 2026-07-28, the 2025 session versions and legacy
  HTTP+SSE; with no auth, a bearer, an API key (header or query), basic, custom headers,
  or OAuth 2.1 (the discovery walk as a timeline of documents, dynamic registration,
  PKCE in a popup for any issuer, client credentials, refresh). The relay, `POST
  /_rustybin/mcp-relay` (SSE back, at most 64 sessions idle 10 minutes, `DELETE
  /_rustybin/mcp-relay/{session}`), makes the calls when the browser cannot: this
  instance's servers in process, other hosts under the egress purpose `mcp_relay`
  (`RUSTYBIN_EGRESS_ALLOW`), and `POST /_rustybin/mcp-relay/fetch` fetches discovery
  documents. **Checks**: `POST /_rustybin/mcp-checks` runs fourteen checks (connect,
  version, auth refusal, tool schemas, every tool called with generated or AI arguments,
  unknown tool and method, bad arguments, resources, prompts, ping, routing headers, the
  server stream, the session lifecycle) with latency, as JSON or JUnit; runs are saved
  (at most 50). Saved connections (`/_rustybin/mcp-connections/{id}`, admin, at most 50)
  keep their credentials sealed with the settings key and used only by the relay. Two
  assist tasks: `tool_arguments` (Generate arguments, validated against the schema) and
  `sampling` (answer a server's `sampling/createMessage` with your provider). New book
  page [MCP inspector](mcp/inspector.md).
- **MCP registry: a mock registry and inspecting real ones** (step 06). A mock MCP
  registry at `/mcp-registry` in the registry API's `/v0.1` shape (`/v0` answers the
  same): `GET /v0.1/servers` with `cursor`, `limit`, `search`, `updated_since`,
  `version` and `include_deleted`, `GET .../servers/{serverName}/versions[/{version}]`,
  `POST /v0.1/publish` with a token from the `auth/*` stubs (`github-at`,
  `github-oidc`, `oidc`, `dns`, `http`, `none`), the status `PATCH` routes,
  `POST /v0.1/validate`, `health`, `ping`, `version`. It lists every MCP server of the
  instance (`io.rustybin/mcp` and its variants, `io.rustybin/manage`,
  `io.rustybin/mock-<name>` per serving mock, remotes under the request's origin), the
  new `registry.servers` section of `rustybin.yaml` and published entries (at most 256
  together). `server.json` documents are validated against the vendored 2025-12-11
  schema plus the registry's semantic checks. The setting `registry.scenario`
  (`RUSTYBIN_REGISTRY_SCENARIO`) breaks it on purpose: `empty`, `large` (2000
  synthetic entries), `flaky` (every third list `503`), `slow` (2 s), `stale`,
  `invalid`, `auth_fail`. Control plane: `GET /_rustybin/registry`, `DELETE
  /_rustybin/registry/servers/{serverName}` (admin), `POST /_rustybin/registry/validate`
  and `GET /_rustybin/registry/browse`, which fetches a real registry under the egress
  purpose `registry` (`registry.modelcontextprotocol.io` and the saved registries,
  `registry.saved` / `RUSTYBIN_REGISTRY_SAVED`), cached for a minute. The console's new
  **MCP registry** view (Mock registry, Browse a registry; Open in inspector hands a
  remote and its auth to the inspector), the assist task `server_json` ("Write a
  server.json for my mock") and the explain kind `server_entry`. Settings can now be a
  choice (a select in the console). New book page [MCP registry](mcp/registry.md).
- **Dev portal: publish and consume** (step 07). A read-only portal at `/portal` (a
  second SPA embedded like the console, `ui-portal/`) that shows only what was
  published: mock APIs (base URL, authentication with the demo credentials, every
  operation with Try it and curl, the full API reference, downloads in seven formats,
  the mock's MCP server), the mock MCP servers (tools, schemas, `claude mcp add` and
  `mcpServers` snippets), the mock LLM (every provider with a sample and Try it), A2A
  agents, the HTTP tools and the mock MCP registry, with search and a "new this week"
  strip. Publish with `PUT /_rustybin/portal/items/{id}` (`mock:<name>`,
  `mcp:<server>`, `llm`, `a2a:<agent>`, `tools`, `registry`; title, Markdown summary,
  tags, contact, order, credentials), `DELETE` to unpublish, `GET /_rustybin/portal`
  for every item and why a published one is hidden, or the new `portal.items` section
  of `rustybin.yaml`. The portal reads `GET /portal/api/catalog` (published items only,
  ETag). Settings `portal.enabled` (`RUSTYBIN_PORTAL`: `auto`, on once something is
  published), `portal.auth` (`RUSTYBIN_PORTAL_AUTH`: `open`, `password`, `key`),
  `portal.base_url` (`RUSTYBIN_PORTAL_BASE_URL`, a gateway URL for snippets and Try
  it), the branding (`portal.title`, `portal.intro` in Markdown, `portal.accent`,
  `portal.contact`) and `portal.visitors` (`RUSTYBIN_PORTAL_VISITORS`: visitors publish
  their own mocks). Insights: views per day for 30 days, downloads by format and Try it
  calls per item, no visitor identifiers (`GET /_rustybin/portal/insights`); items and
  counts persist in `<state>/portal.json`. Console: **Management, Portal** (status,
  branding, items with bulk publish, insights, Preview), **Publish to portal** on a
  mock's page with **Write with AI** (the assist task `summarise_for_portal`), portal
  badges. Settings may be Markdown (a textarea). `/portal/*` is not captured, ignores
  fault headers and is exempt from the key layer. New book page [Dev portal](portal.md).
- **AI agents: who talks to the instance over MCP** (step 08). Every client of the
  management MCP server is an agent, named by its credential (admin token, console
  sign-in, control JWT, OAuth client, API key, visit, or none) and its `clientInfo`
  name: two clients on one token are two agents. `GET /_rustybin/agents` lists them
  (first and last seen, calls today and in total, sessions, status, rights), `GET
  /_rustybin/agents/{id}` adds calls per tool with their outcomes, the last 20 errors
  and the sessions, `GET /_rustybin/agents/events` streams calls (SSE). Rights below
  what the credential allows: a ceiling (`read-only`, `traffic`, `manage`, `full`) and
  tool allow and deny lists, per agent (`PATCH /_rustybin/agents/{id}`), from the new
  `agents:` section of `rustybin.yaml` (`default_ceiling` and `rules` matching the
  client name and credential kind) or the default (`agents.default_ceiling`,
  `RUSTYBIN_AGENTS_DEFAULT_CEILING`, `full`); a tool outside them is not listed and
  answers a tool error. `POST /_rustybin/agents/{id}/revoke` refuses the agent's calls,
  ends its sessions (`403` from then on) and revokes its console and OAuth tokens and
  an OAuth client's refresh tokens; `/restore` undoes it; `DELETE .../sessions` ends
  sessions only. The admin token is never revoked per agent. Control JWTs with a
  revoked `jti` are refused too. Rights, notes, revocations and totals persist in
  `<state>/agents.json`. Audit lines of the management server carry the agent id.
  Console: the **AI agents** view (the list, a detail panel with the rights form, tool
  chips, Revoke, Restore, End sessions, **Explain what this agent did**, the live feed,
  the defaults and rules) now holds **Connect an agent**, moved from Management,
  General. The stdio bridge registers as transport `stdio`.
- **The whole instance as code, and source control** (step 09). `rustybin.yaml`
  version 2 holds the whole instance: `instance` (the console title and user),
  `settings` (stored, non-secret settings by key), `secrets` (references only:
  `${VAR}` from the environment, `secret:stored` for what the instance holds), the
  `mocks`, `registry`, `portal` and `agents` sections as before, `harness` (saved
  scenarios) and `health`; version 1 files still load. `GET /_rustybin/config/export`
  (YAML, JSON, or a `.tar.gz` bundle with the specs as files), `POST
  /_rustybin/config/apply` `{document, prune, dry_run}` answering the diff (`mocks.orders:
  spec changed (2 operation(s) added)`), each section's result and the problems, `GET
  /_rustybin/config/schema`. An absent section is left alone; with `prune` a present
  section is the whole truth. `RUSTYBIN_CONFIG_FILE` loads every section at startup
  and on reload. Source control (`src/sourcecontrol.rs`, the `git.*` settings seeded by
  `RUSTYBIN_CONFIG_GIT*`): a working copy in `<state>/repo` driven by the `git` binary
  (now in the container image, with `openssh-client`), a token through `GIT_ASKPASS`
  or a generated ed25519 deploy key, the remote's host under the egress purpose `git`;
  `GET /_rustybin/git` (status, ahead and behind, the drift a commit would record, the
  timeline), `POST /_rustybin/git/init`, `/commit`, `/push`, `/pull` (fast-forward,
  then apply with a dry run), `/resolve` (a diverged branch: the instance's copy or the
  repository's, never a merge), `GET`, `POST /_rustybin/git/key`, `GET
  /_rustybin/git/history`; `RUSTYBIN_CONFIG_GIT` clones and applies before serving
  (`_REQUIRED` makes a failure fatal); `git.auto` pushes every change or pulls at an
  interval. Console: **Management, Export** (the document, copy, download, bundle, the
  diff since the last export, apply with a dry run and an explicit prune) and
  **Management, Source control** (the repository form, status, connect, commit and
  push with **Write the commit message**, pull with the diff, resolve, the deploy key,
  history, the operations). The mock MCP registry's published entries are now part of
  `AppState` (exported and pruned with the document).
- **The `rustybin` command line for remote instances** (step 10). `rustybin login --url
  <url>` signs in with the console password (or keeps a token with `--token`) into a
  profile (`profiles.json` under `RUSTYBIN_CONFIG_HOME`, else `~/.config/rustybin`, mode
  `0600`; `RUSTYBIN_URL` and `RUSTYBIN_TOKEN` override, `--profile` picks one), then
  nouns and verbs mirror the control API: `status`, `whoami`, `logout`, `instance`,
  `settings`, `sessions`, `password`, `mocks` (every mock verb, the AI ones included),
  `visits`, `visit`, `requests`, `bins`, `agents`, `ai`, `mcp` (servers, saved
  connections, checks with `--junit`, calls through the relay), `registry`, `portal`,
  `config`, `git`, `harness` (the remote verbs; `validate`, `run` and `report` stay
  offline unless `--remote`), `keys`, `egress`, `profiles`, `completion`. Tables for
  people, `--json` for scripts, `--yes` for destructive verbs, `--max` and `--timeout`
  on the tails; exit codes 0, 1 (refused or failed), 2 (usage), 3 (network or
  credentials). A parity test fails when a control route lands without a command; the
  reference `docs/src/cli.md` is generated from the command tree.
- **Management MCP parity and the build-an-instance prompts** (step 11). Every
  control route now has a management tool (or a resource, or a written reason), kept so
  by a parity test over the catalogue: settings (`get_settings`, `update_settings`,
  never secrets), the outbound policy (`check_egress` with a dry check of a URL), AI
  assist (`assist_status`, `assist_usage`, `test_assist`, `run_assist_task` for the
  read tasks) and the AI-built mocks (`generate_spec` with `create`,
  `generate_examples` with `apply`, `edit_spec_with_ai` with `apply`, each confirmed
  when it writes; `get_mock` and `check_mock` carry the coverage), the MCP inspector
  (`list_mcp_servers`, `mcp_call` to any server with the handshake its transport
  needs, `run_mcp_checks`, the saved runs and connections), the mock registry
  (`list_registry_servers`, `set_registry_scenario`, `browse_registry`,
  `validate_server_json`, `unpublish_registry_server`), the dev portal
  (`list_portal_items`, `publish_portal_item` confirmed on an open portal,
  `unpublish_portal_item`, `set_portal_branding`, `get_portal_insights`),
  configuration as code at version 2 (`export_config`, `diff_config`, `apply_config`)
  and git (`git_status`, `git_init`, `git_commit`, `git_push` confirmed, `git_pull`,
  `git_resolve`, `git_deploy_key`), agents (`list_agents`, `get_agent`,
  `set_agent_rights`, `revoke_agent` confirmed and never oneself, `restore_agent`,
  `end_agent_sessions`), sign-in sessions (`list_sessions`, `end_session`) and
  `reload_mocks`. Calls to another host, a browsed registry and the answers of MCP
  servers are `untrusted_*` content; secrets are never tool inputs and never returned.
  Five new prompts walk the common jobs: `build_instance` (from a brief to a published,
  exported instance), `publish_portal`, `backup_to_git`, `test_mcp_server` and
  `onboard_agent`; `mock_an_api` gains the describe path. New resources
  `rustybin://harness.schema.json`, `rustybin://portal/catalog` and `rustybin://cli.md`;
  the server instructions are rewritten around the jobs. `rustybin mcp-stdio` takes
  `--url` and `--profile`, so a local agent manages a remote instance with the sign-in
  of `rustybin login`. The REST handlers of the AI mock routes, the portal, the registry
  and the inspector share their logic with the tools (one code path each). The MCP
  conformance suite walks `build_instance` with the Python SDK.
- **Docs sweep, conformance, release 0.2.0** (step 12). The book is reordered around
  the jobs (Using Rustybin: the console, your own AI, AI-built mocks, the portal, the
  instance as code, source control, the CLI, agents, the MCP inspector and registry,
  load testing, recipes, exports), and every one of those pages ends with an "On every
  surface" table: the same job from the API, the console, the CLI, the management MCP
  and `rustybin.yaml`, with its environment variables. The introduction, the README
  and the quick start tell the 0.2 story (describe, explore, manage, lock) and show the
  sign-in, the AI key, a described mock, the portal and the export in five commands.
  Every 0.2 route has a tested example (new: ending one sign-in session and an agent's
  sessions, the agents feed, the deploy key, connecting a repository through the
  settings and resolving a diverged branch, the registry's health, version, public
  validate, server-level status and `/v0` alias, the portal's API reference page).
  The MCP conformance runner opens the control plane of its first instance again
  (`RUSTYBIN_CONTROL_AUTH=open`, red in CI since the sign-in became the default) and
  generates examples on a mock that lacks them (a described spec has them everywhere).
  The console check covers every 0.2 view. `cargo` version 0.2.0, the tag `v0.2.0`,
  the signed multi-arch image `ghcr.io/coldbathcode/rustybin:0.2.0`, digest
  `sha256:002c7f164c78c06153b90dd5d8cb75b85ac8a72bb6696e038474a43fee42740e` (verify
  it with the cosign command of the [quick start](quick-start.md#docker); the
  [release](https://github.com/coldbathcode/rustybin/releases/tag/v0.2.0) carries
  the same values for a platform's image verification).

## 0.1.0 (2026-10-29): the AI and agent gateway upgrade

A broad rework of Rustybin, from an httpbin-style echo service into an upstream for
API, AI and agent gateway demos.

### Foundation

- axum 0.8, a library plus a thin binary, a shared `AppState`, and a middleware stack
  with request ids, CORS, a time-to-headers timeout, a request body limit and the
  request inspector.
- The route catalogue as the single source of truth for the landing page, the eight
  collection exports, the README tables and the console's API explorer, with tests
  that keep routes, catalogue and OpenAPI in sync.
- Fault injection on every route (`X-Rustybin-Delay`, `X-Rustybin-Fail`), sessions
  (`X-Rustybin-Session`), public mode, an admin token for global mutations, and a
  control plane under `/_rustybin/`.
- Graceful shutdown; the HTTPS and gRPC listeners are optional; invalid settings
  warn instead of failing; bounded state everywhere.

### New

- **Mock LLM** (`/ai/*`): OpenAI (Chat Completions, Responses, completions,
  embeddings, moderations, images, audio), Azure OpenAI, Anthropic, Gemini, AWS
  Bedrock (with binary event streams), Ollama and Cohere, with modes, scripted
  rules, tool calls, structured output, deterministic tokens, native errors and
  rate-limit headers, latency and pace controls, credential checks and request
  inspection.
- **Guardrail mocks**: Azure AI Content Safety, Bedrock ApplyGuardrail, generic
  check and PII redaction.
- **MCP server**: Streamable HTTP for 2026-07-28 and the 2025 handshake versions,
  legacy HTTP+SSE, OAuth 2.1 protected and API key variants, named servers.
- **A2A agents**: seven agents over A2A v1.0 and v0.3, JSON-RPC and HTTP+JSON, with
  streaming, multi-turn, auth-required and SSRF-safe push notifications.
- **Identity provider**: authorization code + PKCE with a login page, refresh token
  rotation, password, client credentials, token exchange, introspection,
  revocation, dynamic client registration.
- **Real auth checks**: JWT validation (HS256 and RS256), HMAC signatures (hmac-auth
  and draft-cavage, with digests), mTLS on the HTTPS listener or forwarded in a
  header, with a persisted demo CA.
- Request bins, generic SSE streams, webhook signing and verification (Standard
  Webhooks, GitHub and Stripe styles), compression (gzip, deflate, Brotli, zstd),
  ranges, data transfer helpers, generic JSON-RPC, GraphQL subscriptions and
  automatic persisted queries, gRPC reflection, health and gRPC-Web.
- The **web console** at `/ui`.
- **OpenAPI mocks** (`/mock/{name}`): upload an OpenAPI 3.0 / 3.1 spec and get a
  named mock API answering with spec examples or deterministic data generated from
  the schemas (path parameters echoed), with rules, per-operation overrides, the
  `Prefer` header, request validation (warn or enforce), deploy checks with a
  report, drafts, rollback, an on/off switch, captured traffic, and mocked security
  from the spec (API key, basic, bearer, OAuth 2.0 / OpenID Connect scopes against
  the built-in identity provider, mTLS) or a fixed scheme, its secrets shown only to
  callers who may change the mock. Managed in the console,
  through `/_rustybin/mocks*`, from `rustybin.yaml` (`RUSTYBIN_CONFIG_FILE`,
  `--config`, reload on `SIGHUP`, JSON Schema for editors) or a directory of specs
  (`RUSTYBIN_MOCKS_DIR`), and with the `rustybin mock` CLI. A `petstore` sample is
  built in (`RUSTYBIN_MOCKS_SAMPLE`). A reload never deletes mocks because of a
  broken file or entry (they keep their last good revision and show a
  `load_error`), enforced validation answers `406` for an unacceptable `Accept`,
  missing scopes get an RFC 6750 `insufficient_scope` challenge, and requests are
  counted in `rustybin_mock_requests_total`.
- **Self-service mocks** for hosted demos (`RUSTYBIN_MOCKS_SELF_SERVICE`, off by
  default): visitors of a public instance create and manage their own mocks
  through a manage token, on a time allowance (an hour, extendable to three, then
  an email address for up to six hours a day, delivered to a webhook), on a
  separate mocks domain, with responses that can never set cookies, redirect
  elsewhere or ship active content. Operators can list, end and pause visits.
- Mock collections also export as a Bruno collection and an Insomnia export
  (`GET /_rustybin/mocks/{name}/collections/bruno|insomnia`, `rustybin mock
  collection`, the console and the management MCP's `export_collection`), in the
  same shapes as `/export/bruno.json` and `/export/insomnia.json`.
- **An MCP server for every OpenAPI mock** at `/mock/{name}/_rustybin/mcp` (and
  `<name>.<domain>/_rustybin/mcp` on the mocks host): one tool per operation, with
  flat arguments from the parameters and body, annotations from the method, an
  output schema from the default response, and the served spec as a resource. Each
  call runs through the mock (rules, examples, stateful data, mocked security with
  the MCP request's credentials, delays, captures marked as MCP), so tools return
  what the REST calls return; a status outside 2xx is a tool error. Settings
  `mcp.enabled` and `mcp.exclude`; an "MCP server" tab in the console.
- **Management MCP server** at `/_rustybin/mcp`: configure a Rustybin from an AI
  agent such as Claude Code. Tools for the instance, OpenAPI mocks (create, edit,
  check, deploy, roll back, delete, collections, `rustybin.yaml` export and apply),
  testing (`try_request` in process on this instance only, `issue_token`,
  `validate_payload`, `wait_for_requests`), captured traffic, usage and
  self-service visits, plus three prompts and two resources. Protected by
  control-plane authentication; `tools/list` shows only what the caller may use;
  destructive tools ask for confirmation (elicitation, else `confirm: true`);
  captured traffic is returned only under `untrusted_*` fields, truncated; changes
  are logged without arguments and counted in `rustybin_manage_mcp_calls_total`.
  The console shows the `claude mcp add` command for the instance, and
  `rustybin mcp-stdio` bridges stdio-only clients.
- **OAuth sign-in to the management MCP server**: where the control plane asks for
  credentials, agents sign in through the browser instead of pasting a token. A
  built-in authorization server (RFC 9728 and RFC 8414 metadata, dynamic client
  registration, PKCE, refresh tokens) sends the browser to a consent page in the
  console, which grants at most what the approver's own credential allows. Tokens
  open `/_rustybin/mcp` only. `RUSTYBIN_CONTROL_OAUTH`, `RUSTYBIN_CONTROL_OAUTH_KEY`,
  and `RUSTYBIN_CONTROL_OAUTH_ISSUER` for an external authorization server.
- **Plan limits** for hosted offerings (`RUSTYBIN_PLAN`, `RUSTYBIN_LIMIT_*`,
  `RUSTYBIN_USAGE_FILE`): rate, concurrency, streams and daily or monthly request and
  egress quotas, per instance or per session, with IETF RateLimit headers on 429s and
  `GET /_rustybin/usage`. Off by default.
- This documentation site, with every example tested in CI, and SDK conformance
  suites for the mock LLM, MCP, A2A and OAuth.

### Operations and hosting

- The management MCP server no longer passes the caller's control credentials
  into the requests its tools send to the data plane, and captures (inspector,
  bins, mock traffic) store `X-Rustybin-Admin-Token` and `X-Rustybin-Mock-Token` as
  `[redacted]`. Mock traffic now redacts API keys like the inspector does.
- A workflow publishes the image to `ghcr.io/coldbathcode/rustybin` on pushes to
  `main` and on `v*` tags.
- Control-plane authentication (`RUSTYBIN_CONTROL_AUTH`): `token` (admin token) or
  `jwt` (admin token or Ed25519 JWTs with `aud`, `exp` and the scopes `inspector`,
  `console`, `admin`) for every `/_rustybin/*` route except the new readiness probe.
  The web console signs in with a token from the URL fragment or a sign-in screen
  and reads its live feed with `fetch` so the bearer header is sent.
  `RUSTYBIN_HOSTED_MODE=true` presets `jwt` and JSON logs.
- `GET /_rustybin/ready`: readiness, always 200 while serving, independent of the
  `/health` demo toggle, exempt from auth, plan limits and capture.
- `GET /_rustybin/metrics`: Prometheus metrics with bounded labels (route templates,
  status classes, protocols, mock LLM tokens by model family, injected faults,
  open streams, egress, build info).
- More metrics: `rustybin_limit_rejections_total{dimension}` (plan limiter 429s and
  gRPC `RESOURCE_EXHAUSTED`), `rustybin_llm_requests_total{provider,model_family,streaming}`
  and `rustybin_llm_faults_total{provider,kind}` (mock LLM faults by kind), and
  latency buckets from 1 ms to 60 s on `rustybin_request_duration_seconds` so p50
  and p99 can be computed with `histogram_quantile` (PromQL examples in the docs).
- `RUSTYBIN_LOG_FORMAT=json`: one JSON object per line with the request id.
- `RUSTYBIN_GRPC_ON_HTTP=true`: the gRPC services on the HTTP and HTTPS listeners
  (h2c and ALPN `h2`) for a TLS-terminating proxy.
- `RUSTYBIN_CONSOLE_TITLE` and `RUSTYBIN_CONSOLE_BACKLINK` in the console header.
- `/_rustybin/version`, `/_rustybin/status` and `/_rustybin/usage` report the build
  commit (`git_sha`) and the control auth mode.
- Shared mode (`RUSTYBIN_SHARED_MODE=true`): one instance for many API keys
  (`X-Rustybin-Key` or `?rb_key=`) from a registry file or URL, per-key limits and
  daily quotas (429 `rustybin_quota_exceeded` until 00:00 UTC), inspector and usage
  scoped by key, keys redacted in captures, readiness waiting for the registry, and
  `GET /_rustybin/keys/usage` for collectors. `RUSTYBIN_REQUIRE_KEY=true` requires a
  key on a normal instance.
- Optional cargo feature `quota-postgres`: replicas share daily quotas through
  PostgreSQL (`RUSTYBIN_QUOTA_STORE`), with `rustybin_quota_store_up` in the metrics.
- **Harness mode, single node** (`RUSTYBIN_HARNESS=on`, phase 1 of the harness plan):
  Rustybin as a load client with its own engine runtime, a dedicated scheduler
  thread, an in-flight cap from the descriptor limit and saturation detection, so a
  slow client is never read as a slow gateway. Scenarios (targets with weights,
  request templates with placeholders, open and closed load models, `constant` and
  `ramp` patterns chained with `then`, thresholds) are validated against the node's
  caps and one JSON Schema (`GET /_rustybin/harness/schema`) drives the API, the
  management MCP tools (`validate_scenario`, `create_run` with confirmation,
  `get_run_report`, `harness_stop`) and the console's scenario builder. Targets must
  be in `RUSTYBIN_HARNESS_TARGETS` and are pinned to their resolved addresses;
  link-local, metadata and control-plane targets are always refused; optional target
  verification through `/.well-known/rustybin-harness`; a kill switch at
  `POST /_rustybin/harness/stop`; the `harness` control scope. Reports as JSON, text
  and JUnit; headless runs at startup with exit codes; `rustybin harness run |
  validate | report`.
- **Harness correlation** (phase 2): every harness request carries a signed marker
  (`X-Rustybin-Harness`), an `X-Request-Id` and a `traceparent`; a server layer
  records arrivals, processing time and what the gateway changed (headers added,
  removed, rewritten, bodies, paths), samples captures into the inspector and tags
  every answer (`Server-Timing: rb`, `X-Rustybin-Served-By`). The client classifies
  answers as `upstream`, `gateway` (by status) or `transport`, counts `lost`
  requests after the run, and reports the gateway overhead and, in loop-through,
  one-way legs. Thresholds `gateway_overhead_p99_ms`, `lost`, `upstream_reach`; the
  scenario's `servers` block drives the shaped upstream `/harness/{*path}` (log-normal
  latency, status mix, body size, chunked streaming) and inspector sampling;
  `rustybin_harness_*` metrics with a bounded run label.
- **Harness clusters** (phase 3): a coordinator creates a cluster
  (`POST /_rustybin/harness/clusters`, or `RUSTYBIN_HARNESS_CLUSTER` with
  `RUSTYBIN_HARNESS_JOIN_TOKEN`); client and server nodes join with the token
  (`RUSTYBIN_HARNESS_JOIN`, `POST /_rustybin/harness/join`) and heartbeat with a long
  poll, carrying cumulative stats and NTP-style clock stamps. Runs are submitted, then
  started once the quorum is there (`scale.client_nodes`, `scale.server_nodes`, the
  cluster's `min_nodes`; headless `RUSTYBIN_HARNESS_START=on-quorum`): the rate is split
  by declared capacity, virtual clients by disjoint ranges, the start is synchronised,
  and one report merges every node's exact histograms, with `lost` checked against
  every server node. Leases stop a client that loses its leader. The server nodes are
  exported for the gateway's upstream pool (`GET .../clusters/{id}/upstreams`); members
  redirect controllers to the leader. MCP tools `create_cluster`, `list_cluster_nodes`,
  `export_upstreams`, `start_run` (and `cluster_id` on the run tools); a cluster card in
  the console.
- **Harness failover** (phase 3b): nodes with `RUSTYBIN_HARNESS_CONTROL_URL` are
  eligible to lead (`RUSTYBIN_HARNESS_LEADER=prefer` makes a standby). The leader
  replicates a versioned snapshot (members, runs with a checkpoint of their stats,
  secrets to eligible nodes only) and orders the live eligible nodes by work; when it
  goes silent for the election timeout (`RUSTYBIN_HARNESS_ELECTION_TIMEOUT`, 3 s), the
  first in line is elected by a majority of the eligible nodes and takes over within
  the lease, so clients never stop. Every message carries the term; an old leader
  steps down, a leader without a majority stops renewing leases (a minority partition
  stops sending). The new leader hands its own client share back and re-splits the
  share of silent clients once their lease expired, never sending more than planned;
  what an old leader's own part sent after its last checkpoint is reported as
  `unreported`. `GET .../clusters/{id}/leader` on any member, vote and announcement
  node routes, the MCP tool `get_leader`, and the leader, term, history and succession
  in the console.
- **Harness patterns and live tuning** (phase 4): layers run every pattern
  (`constant`, `ramp`, `step`, `target`, `breakpoint`, `spike`, `pulse`, `burst`,
  `wave`), chained with `then:`, with `jitter` and `sync: cluster | node`; the scheduler
  follows the exact curve second by second and lands a burst within its window. Closed
  loops: `target` on `overhead_p99_ms` moves the rate toward the gateway overhead goal,
  `breakpoint` stops at the first plateau that fails `until` and reports the last good
  rate. `PATCH .../runs/{run}` changes a running run (rate multiplier, pause and resume,
  hold, skip stage, `set` on any pattern field, add and remove layers), checked against
  the caps, logged, and carried in the cluster snapshot through a failover;
  `GET .../runs/{run}/events` is the run's feed (server-sent events). Reports gain the
  changes, the findings and every stage as it ran. `POST /_rustybin/harness/preview`
  returns the expected curve; saved scenarios (`PUT` and `DELETE
  /_rustybin/harness/scenarios/{name}`, `RUSTYBIN_HARNESS_STATE_DIR`) and six new
  built-ins. `scale.client_nodes: auto` waits for nodes and emits `scale_request`, also
  POSTed to a signed controller hook (`RUSTYBIN_HARNESS_SCALE_HOOK`), with
  `scale.scale_timeout`; `scale.connections_per_client`. MCP tools `list_patterns`,
  `preview_scenario`, `save_scenario`, `update_run`; in the console, a preview chart,
  saved scenarios, live run controls and the feed.
- **Harness rate-limit tests** (phase 5): a layer's `expect` (`limit` per `window`,
  `per` key, ip, consumer, route or global, a `burst` allowance, the refusal `status`, a
  `node_header`) and the `limit_test` pattern with ten tests: `over_limit`,
  `window_edge` (bursts around window boundaries on the wall clock), `just_under`,
  `distributed` (every client node at the same window start, over every target),
  `many_keys` and `many_ips` (identity pools: `{{id}}`, `{{ip}}`, rotating forwarded
  addresses from 198.18.0.0/15), `retry_wave` (refusals retried at once or after
  `Retry-After`), `concurrency`, `slow_rise` and `limit_probe` (a closed loop: a binary
  search for the effective limit), each with an optional `recover` stage. The report's
  `rate_limits`: admission per window and in the worst window-long span, over- and
  under-admission, the first refusal, recovery, the measured window, identities and
  gateway nodes, the rate-limit headers (IETF `RateLimit` drafts, `X-RateLimit-*`,
  `Retry-After`), upstream protection from the server side's arrivals by layer,
  collateral damage to the other layers, and findings; per-layer `thresholds`
  (`rate_limited`, `over_admission`, `under_admission`). Counted on every client node
  and merged by the leader. Built-ins `rate-limit`, `limit-probe` and `many-ips`; the
  analysis in the console's report and the MCP report summary.
- **Harness mock-driven traffic and identities** (phase 6): a layer's source can be an
  OpenAPI mock (`source: { mock: petstore }`): every operation of its live revision, with
  generated parameters and bodies, `operations` (include, exclude, weights),
  `invalid_fraction` (requests that expect a 4xx), `stateful` (a create followed by a
  read of what it made) and `base_path`. A layer's `identities`: API keys (listed or
  minted), JWTs minted by the node's built-in IdP (RS256 or the shared HS256 demo
  secret), OAuth client credentials against an allowlisted token endpoint (fetched
  before the run, cached, renewed), forwarded-address pools and source address binding.
  The report breaks the answers down by operation and by credential (labels without
  secrets) and says when invalid requests got through; listed keys and client secrets
  are redacted. The `petstore-customers` built-in.
- **Harness faults and protocols** (phase 7): a `faults` block with presets (`calm`,
  `jittery`, `flaky-upstream`, `retries`, `synchronised`, `churn`, `all`) and the live
  change `faults`. Client faults by fraction: connection churn, abandoned requests, slow
  bodies, malformed JSON, wrong content types, oversized headers, immediate (or
  synchronised) retries on 5xx, expired tokens, an HTTP/2 mix. Server faults on the
  run's marked requests: drops after the headers, stalls, flapping `/health`, and node
  churn (the server nodes down in turn, 503 with the connection closed). Protocol
  sources: `llm` (streamed chat completions timed token by token), `sse`, `websocket`
  (upgrade and masked frames on the pinned connections), `grpc` (unary echo over HTTP/2,
  judged by `grpc-status`) and `mcp` (stateless `tools/call`). The report's `streams`
  (time to first token, inter-token latency, tokens per second, stalls), the injected
  faults by name, `abandoned` and `unattributed` counts. Built-ins `llm-streaming`,
  `flaky-upstream` and `jittery-clients`.
- **Harness docs and conformance** (phase 8, the last): the guide
  [Load testing through a gateway](harness.md) with tested examples for the control API
  (status, schema, validate, preview, a run with a live change and its report in three
  formats, saved scenarios, the cluster API on one node, the kill switch) and the CLI;
  a compose cluster (`docs/examples/harness/cluster`: a leader, a standby, two clients,
  two servers and a stand-in gateway, headless on quorum); `conformance/harness/run.sh`,
  which runs that cluster, kills the leader mid-run and asserts the standby's report; the
  harness tools, prompt and resource on the agent page. The harness plan is complete.
  Found by that run and fixed: a member that followed a new leader's announcement stood
  for election itself at once, and candidates that split the votes retried in lockstep;
  following a leader now defers the node's next stand, retries add a random wait, and
  refused votes are logged with their reason.

### Changed

- Open redirects and other abuse-prone endpoints were removed or made safe
  (`/redirect-to` only redirects to the same host).
- Content negotiation serves JSON to browsers by default.
- The Docker image no longer generates certificates with OpenSSL at startup:
  Rustybin generates and persists its own demo PKI (the old script produced a CA
  that the server replaced, which broke mTLS and HTTPS verification in containers).
- `tests/smoke_test.sh` was replaced by the documentation examples
  (`docs/examples/run.sh`), which need Hurl 8 (CI pins 8.0.1).
- The Docker image runs as an unprivileged user (uid 10001) and supports a
  read-only root filesystem; `docker-compose.yml` keeps the demo PKI in a named
  volume, and `fly.toml` uses ports 8080 / 8443 and the readiness probe.
- MCP `tools/list` (and the other list methods) render their result once per server
  and protocol version instead of on every request: about 7x the throughput, same
  bytes on the wire.
- Minimum supported Rust version is now 1.88 (was 1.86), to take security fixes in
  `ruzstd` (zstd request bodies, RUSTSEC-2026-0344) and `time` (used when parsing
  client certificates, RUSTSEC-2026-0009).
- Security updates: rustls 0.23.46 (RUSTSEC-2026-0285, -0346, -0347, -0348, -0349),
  rustls-webpki 0.103.15 (RUSTSEC-2026-0104), h2 0.4.20 (RUSTSEC-2026-0258) and
  quick-xml 0.42 (RUSTSEC-2026-0194, -0195: memory exhaustion in the namespace reader
  used by the SOAP endpoint). SOAP field values still decode `&amp;`, character
  references and the other predefined entities; unknown entities are a Sender fault.
- CI runs `cargo audit` on every push.

### Fixed

From a full code review (2026-10-25):

- **GraphQL**: a query nested thousands of levels deep overflowed the parser's stack
  and aborted the whole process. Queries nested more than 64 levels are refused before
  parsing (`NESTING_LIMIT_EXCEEDED`, WebSocket close 1008).
- **Credentials in captures**: `Authorization: Bearer` with the admin token or a
  control JWT is captured as `[redacted]`; the `rb_key` query is redacted in OpenAPI
  mock captures; AI request records redact `X-Rustybin-Key` and `rb_key` and, in
  shared mode, are scoped by key.
- **Console**: the API explorer shows the console's own credentials masked; a valid
  token with a narrow scope signs in; the admin token is kept for the tab only.
- **Shared mode console**: a token bound to a key (`inspector` scope, `rb_key`) was
  sent back to the sign-in screen when an admin token was configured: the console's
  `GET /_rustybin/status` and every other route outside the key's reach answered `401`.
  The status now answers a key with the key's view, and a valid key gets `403` on the
  routes it may not use (`401` stays for a missing or invalid credential). The
  console's own data-plane requests send the token as `X-Rustybin-Key-Token` (it
  stands in for the key, redacted in captures), its traffic view no longer narrows a
  key's requests to the console session, the overview shows the key's view, and the
  API explorer may read the route catalogue.
- **MCP**: tool scopes are checked inside JSON-RPC batches on `/mcp/protected`; the
  legacy HTTP+SSE transport caps requests in flight per session.
- **Self-service**: redirects that browsers read as off-domain (`/\host`) are refused;
  a trailing-dot `Host` no longer reaches the console on the mocks domain; rotating a
  manage token can no longer drop the visit's mocks; every saved visitor mock loads
  after a restart; a visitor mock's spec answers only on the mocks domain.
- **OpenAPI mocks**: odd path templates, multibyte response keys and huge integer
  bounds no longer panic; validation, `minLength` and stateful collections are bounded.
- `/status` with weights whose sum overflows answers `400`; the Azure-shaped
  `text:analyze` caps blocklist names and repeated rows; `/response-headers` keeps
  reflected values inert under any `Content-Type`.
- File-defined mocks load before the HTTP listener serves; JWT control auth warns when
  the token audience would change on every restart; `/openapi.json` is built once.
