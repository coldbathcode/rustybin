# Step 06: Mocks, checkpoints and tests: pools, locks, persistence

| # | Severity | Finding | Anchor | Fix |
|---|---|---|---|---|
| 1 | medium | Token-less callers are not compile-rate-limited, and `create` compiles before the admission checks (`preview` has none) | `src/mocks/service.rs:674`, `:942` | admission and rate before the compile, keyed by the caller's ledger; charge `preview` |
| 2 | medium | The AI edit preview and `duplicate` compile outside the compile pool; `source: generated` examples skip the AI checks | `src/mocks/ai.rs:417`, `:612`, `src/mocks/service.rs` (`duplicate`) | `Svc::compiling`; the visitor compile allowance |
| 3 | medium | Visits state is serialised whole under its lock, which the visitor data plane takes per request; `note_creation` grows the ledger past its cap | `src/mocks/visits.rs:400`, `:565`, `:785` | serialise a copy outside the lock; the ledger cap there too |
| 4 | medium | Checkpoint restore and save take no lock: two restores interleave; a failed recreate after the delete loses the mock | `src/checkpoints.rs:317`, `:392` | one restore at a time under the edit lock; build first, then swap |
| 5 | low | Mock persistence (an fsync per change) runs on async workers | `src/mocks/store.rs:452`, `:483` | the blocking pool |
| 6 | low | Revision numbers restart at 1 on boot, so an `If-Match` can match a different draft | `src/mocks/store.rs:535` | persist the counter or add an epoch to the ETag |
| 7 | low | `PATCH` with `deploy: true` deploys after the edit lock is dropped (a newer draft can go live) | `src/mocks/service.rs:1108` | deploy the revision just written, under the lock |
| 8 | low | `Tests::put` and `delete` answer success when the file write fails | `src/replay/mod.rs:401`, `:411` | return the error |
| 9 | low | `run_tests` replays `POST` and `DELETE` in process unconfirmed, and runs are not single-flight | `src/mcp/manage/replay.rs:162`, `tools.rs` | confirm runs with non-GET tests; one run at a time |
| 10 | low | A scenario run id has 32 bits, and its sessions can be forged to skew a check | `src/scenarios/mod.rs:348` | 96-bit ids and a per-run secret in the session prefix |
| 11 | low | The per-mock MCP server builds the `Cookie` header without encoding values | `src/mcp/openapi.rs:493` | percent-encode |
| 12 | low | `mcp-checks`, `run_mcp_checks`, `mcp_call` and the relay have no concurrency cap | `src/mcp/inspector/checks.rs:303`, `client.rs:28` | a small semaphore |
| 13 | low (verify first) | In-process requests all use the peer 127.0.0.1, so tool users share per-address limits and `ip:` sessions | `src/mcp/manage/testing.rs:143`, `src/replay/run.rs` | a synthetic per-caller peer, or exempt in-process requests from per-address limits |

## Build notes (2026-11-02)

Every finding is fixed; none closed without a fix. The tests named below fail on the
code before this step (they either call what this step adds or assert the behaviour it
changes).

1. **Compile rate and admission.** `Svc::compiling` now charges every caller without
   admin rights (`Svc::compile_key`): the visit, else the API key, else the address
   part of the ledger key (`anon:ip:<addr>`), so a caller without a manage token, and
   one rotating `X-Rustybin-Session`, is counted; `preview` (`/_rustybin/mocks/import`)
   goes through it as before and is now charged. `create` runs the visitor's caps,
   the pause check and the creation rate (`visitor_caps`, which counts the creation)
   and the name given up front (`visitor_name`) before the compile; the name derived
   from the spec is checked after it. Tests
   `callers_without_a_manage_token_spend_a_compile_allowance_too`,
   `a_creation_past_the_caps_is_refused_before_its_spec_is_compiled`.
2. **Compile pool.** The AI spec preview (`spec_work`), the AI edit preview
   (`edit_work`), `duplicate` (REST handler and `duplicate_mock`, now async) and the
   generated examples (`source: generated`: the whole generator loop is one
   `Svc::compiling` call, so it is off the async workers and spends the visitor
   allowance) all compile on the pool. `Svc` is `Clone` for the calls that use the
   pool twice. Test
   `duplicates_and_generated_examples_compile_on_the_pool_with_the_allowance`.
3. **Visits.** `save_unlock` clones the saved part of the state under the lock
   (`Inner::saved_copy`), drops the lock, then serialises and writes; each copy has a
   change number taken under the lock and the writer drops a copy older than the one
   written (`Writer::offer` / `flush`), so the latest change wins. The write itself
   runs through `off_runtime`. `ledger_entry` refuses to add a new address once
   `sessions_per_address` holds `MAX_LEDGER` (the address shares `<addr>/*`), so
   `note_creation` no longer grows it. Tests
   `counting_creations_never_grows_the_address_ledger_past_its_cap`,
   `saves_written_off_the_lock_keep_the_latest_state`.
4. **Checkpoints.** `restore` (dry run included) and `save` hold
   `MockStore::edit_lock` from the first read to the last write: one restore at a
   time, never an edit in the middle. Each mock is compiled first and swapped in with
   `MockStore::restore_replacing` (one write lock, the replaced mock not counted in
   the caps), so a mock that cannot be restored keeps what it had and the answer
   names the problem. `covered` also excludes scratch mocks. Test
   `restores_run_one_at_a_time_under_the_edit_lock` (an edit holds the lock: the
   restore waits; three racing restores report no problem, ten times);
   `a_restore_replaces_a_mock_in_one_write_within_the_cap`.
5. **Persistence off the async workers.** `store::off_runtime` runs the file work of
   `persist_now` and of the expired-mock removals in `write()`: on a worker of the
   multi-threaded runtime through `block_in_place` (the worker hands its tasks over),
   elsewhere in place. Chosen over `spawn_blocking` so a mutation stays synchronous
   (its save is on disk when it returns; the restart tests rely on that). The visits
   writer and the saved tests' file use it too. Test
   `file_work_leaves_the_async_workers` (one worker: a timer task still fires while
   another blocks 400 ms).
6. **Revision numbers.** The counter of a store starts at the milliseconds since
   2026-01-01 (`first_revision`) instead of 1: numbering continues across restarts
   (a process would have to make more than a thousand revisions a second to catch
   up with the clock), so an `If-Match` from before a restart never matches. No file
   and no ETag format change; revisions are larger numbers. Test
   `revision_numbers_continue_across_a_restart`.
7. **PATCH with `deploy: true`.** The deploy now happens under the edit lock with
   `MockStore::deploy_if(name, force, Some(revision just written))`, so a newer draft
   is never published by it (a writer outside the edit lock that changed it meanwhile
   gives the stale refusal). Tests `a_patch_with_deploy_publishes_the_revision_it_wrote`,
   `a_deploy_naming_a_revision_refuses_another_draft`.
8. **Saved tests' writes.** `Tests::change` builds the next set under the save lock,
   writes the file (off the items lock, so reads go on) and only then swaps it in; a
   failed write answers `StoreError::Io` (500) and changes nothing. `delete` returns
   `Result<bool, StoreError>`; the REST handler, `delete_test` and the configdoc tests
   section report it. Test `a_save_or_delete_the_file_refuses_changes_nothing`.
9. **`run_tests`.** Confirmed when a selected test sends a method other than `GET`,
   `HEAD` or `OPTIONS` (the tests are named) as well as when one goes through a
   gateway. Runs are single-flight on every surface: `Tests::begin_run` (an atomic
   flag and a guard) in the REST run handler (`409`) and the tool (an error); the
   OpenAPI documents the 409. Tests
   `run_tests_asks_before_replaying_a_request_that_changes_state`,
   `one_run_of_the_saved_tests_at_a_time`.
10. **Scenario run ids.** 96-bit run ids (24 hex; the mock `scn-<id>` stays within
    40 characters) and a 64-bit per-run secret in every session of the run's requests
    (`scn-<id>-<secret>-<check>-<n>`, also the flaky counters' prefix), never shown
    (the view's `sessions` says `scn-<id>-*`). Test
    `a_run_cannot_be_guessed_nor_its_check_skewed_by_forged_sessions` (requests sent
    to the upstream under the sessions the old scheme used do not change a caching
    verdict).
11. **Cookies.** The per-mock MCP server percent-encodes every byte RFC 6265 does not
    allow in a cookie value (`;`, `,`, space, quotes, backslash, controls, non-ASCII)
    and `%` itself, plus `=` in a name (`encode_cookie`); ordinary values pass
    unchanged. Test `a_cookie_argument_cannot_add_another_cookie`.
12. **Inspector caps.** `Inspector.check_runs` (2 at once: `run_and_save`, now
    `Option`, shared by `mcp-checks` and `run_mcp_checks`) and `Inspector.calls` (16:
    each relay exchange, held until its SSE answer ends, legacy included, and each
    `mcp_call`). Past them `503` with `reason: busy` (REST) or a tool error. Test
    `mcp_checks_and_calls_have_a_concurrency_cap`.
13. **In-process peers (verify first: confirmed).** `try_request` and the traffic
    tools' in-process requests, and saved test runs, carried `127.0.0.1`, so every
    tool user shared per-address limits and the `ip:127.0.0.1` session (the test sees
    `/ip` answer `127.0.0.1` for two callers on today's code). Now
    `session::in_process_peer`: the caller's own address (the MCP request's client
    address), else the address of an `ip:` session key (the REST run of a caller
    without a session header), else a synthetic unique-local address made from the
    session. The one line in `replay::run::direct` is the only change there. The
    inspector's in-process requests (`inspector::client::send_local`) still use
    loopback: they carry no caller, and the relay is now capped (finding 12). Test
    `in_process_requests_carry_the_callers_own_address`.

Extras:

- **Step 02 finding 10 (scratch mocks).** Instead of "skip mocks with an expiry"
  (which in public mode is every API mock, 24 hours), a scratch mock is marked:
  `Entry.scratch` / `Snapshot.scratch`, set by `create_scratch` in the same write that
  adds it (its expiry too, no second write). `persist_now` never saves one, the
  configdoc export and `GET /_rustybin/mocks/export` leave it out, checkpoints do not
  cover it. Tests `a_scratch_mock_is_never_saved`,
  `an_export_leaves_out_a_scenarios_scratch_mock`.
- **Step 05 finding 11 (`apply_keeping`).** Each definition is one edit under
  `edit_lock`; an update is `replace_deployed`: the draft, the deploy (forced) and on
  or off in one write, refused as stale if the draft is no longer the one read. Test
  `an_apply_waits_for_an_edit_in_progress`.
- **Step 05 finding 1 (prune scope).** `Snapshot::prunable` (instance owner, not a
  scratch mock) in all four places: `MockStore::apply_keeping`, `Svc::apply` (through
  it), `Svc::apply_dry_run` and the configdoc dry run's `would_delete`. The rule uses
  the scratch mark rather than "no expiry", so a public instance's own API mocks
  (which expire after 24 hours) stay prunable. Test
  `a_prune_leaves_visitors_keys_and_scenarios_mocks_alone` (a visit's, a key's and a
  scenario's mock survive the dry run, the configdoc apply and the mocks API apply;
  it failed with the filters removed).
- **Step 08 finding 7 (scenario run owner).** Decision: keep runs owned by the session
  key, and show the credential-injection key only to admin callers. Starting, checking
  and ending already need admin rights, and an admin can read the key on the run's
  mock anyway, so the key is only at risk with callers without admin rights who share
  the session key (one address behind a NAT, a console-scope token, an open-mode
  instance has no such caller). `list` and `get_run` take `reveal` (REST: the
  caller's `admin`; MCP: `Right::Admin`); without it the setup has a placeholder and
  `key_hidden: true`. Requiring `X-Rustybin-Session` to start a run was rejected: the
  console does not send one by default. Test
  `the_injected_credential_is_shown_to_admins_only`.
- **Deploy revision (console request, also finding 7).** `POST
  /_rustybin/mocks/{name}/deploy` takes `If-Match` or an optional body
  `{"expected_revision": n}` (both must agree), checked under the store's write lock
  (`deploy_if`); `409 rustybin_stale_revision` with `changed`. Five surfaces: API and
  OpenAPI (the header, the optional body, 409 and 422), console (already sends
  `If-Match`), MCP `deploy_mock.expected_revision`, CLI `mocks deploy
  --expect-revision N` (default: no revision, the draft as it is; `docs/src/cli.md`
  regenerated), docs (`docs/src/mocks/index.md`, the Hurl examples in
  `docs/examples/mocks/mocks.hurl`: the draft deploy sends `If-Match`, the conflict
  section shows a refused deploy). Tests `a_deploy_names_the_revision_it_publishes`,
  `deploy_mock_refuses_a_draft_it_did_not_name`,
  `mocks_deploy_can_name_the_revision_it_publishes`.

### For the changelog

- `POST /_rustybin/mocks/{name}/deploy` accepts `If-Match` or `expected_revision` and
  answers `409` when the draft moved on (`deploy_mock.expected_revision`, `rustybin
  mocks deploy --expect-revision`). `PATCH` with `deploy: true` publishes exactly the
  revision it wrote.
- Mock revision numbers start from the clock and keep growing across restarts (they
  are large numbers now).
- Callers without admin rights spend the 30 compiles a minute whether or not they
  hold a manage token (per address before a visit); duplicates, AI previews and
  generated examples count; a visitor's creation past the caps is refused before the
  compile.
- A prune never deletes a visitor's, an API key's or a running gateway scenario's
  mock; a scenario's mock is never saved, exported or kept in a checkpoint.
- Checkpoint restores run one at a time and never lose a mock they cannot restore.
- Saving or deleting a saved test answers `500` (nothing changed) when the file
  cannot be written; one run of the saved tests at a time (`409`); `run_tests` asks
  before replaying a `POST`, `PUT`, `PATCH` or `DELETE`.
- Gateway scenario run ids are 96 bits; a run's check cannot be skewed by forged
  sessions; the credential-injection key is shown to admin callers only
  (`key_hidden`).
- The MCP inspector runs at most two check suites and 16 relay calls at once (`503`).
- In-process requests of the management tools and of saved test runs carry the
  caller's address, not `127.0.0.1`.
- Cookie arguments of a mock's MCP tools are percent-encoded.
