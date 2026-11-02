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
