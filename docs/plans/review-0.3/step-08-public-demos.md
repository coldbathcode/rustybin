# Step 08: Public demo modules: per-client caps and leftovers

| # | Severity | Finding | Anchor | Fix |
|---|---|---|---|---|
| 1 | medium (verify first) | A2A tasks are bounded by count, not bytes: 2,000 tasks with 1 MiB messages and growing histories | `src/a2a/tasks.rs:360`, `:417`, `src/a2a/wire.rs:309` | byte caps per message, task and in total |
| 2 | medium | Anyone fills the public bin store by rotating `X-Rustybin-Session` (new bins refused for an hour) | `src/request_bin.rs:255` | per-address creation, as visits |
| 3 | medium | Flooding the demo IdP evicts revocations (a revoked token works again), codes and clients | `src/oidc/store.rs:53` | refuse rather than evict revocations; a per-address registration rate |
| 4 | low | `/response-headers` and `/cookies/set` plant cookies (with `Domain`) on a public origin | `src/response_shaping.rs:172`, `src/cookies.rs:78` | in public mode drop `Set-Cookie` and `_domain` |
| 5 | low | Public flaky counters are evictable by session rotation | `src/flaky.rs:97` | a per-address cap |
| 6 | low | The registry's open `/auth/none` mint evicts an admin's publish token at 256 | `src/mcp/registry/mod.rs:292` | a per-address mint rate |
| 7 | low | A gateway scenario run belongs to the session key; without `X-Rustybin-Session` that is the client address, so everyone behind one NAT sees the run and its credential-injection key | `src/scenarios/mod.rs` | show the key to the starting session only once, or bind runs to the credential |
| 8 | low | The CLI test that drives every command occasionally hangs in the full parallel run (not reproduced in 52 runs on 2026-11-02) | `src/cli/tests.rs` | a per-command timeout in the test, so a hang names its command |
