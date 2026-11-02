# Step 07: Harness: leadership, the kill switch, node calls

| # | Severity | Finding | Anchor | Fix |
|---|---|---|---|---|
| 1 | medium | Any member can depose the leader: a heartbeat with a higher term marks it stale with no sender check, and the leader branch of `elected` only authenticates; the leader then follows the caller's URL with its credential | `src/harness/cluster.rs:2402`, `src/harness/api.rs:1035`, `src/harness/election.rs:42` | eligibility, `MAX_TERM_JUMP` and `check_join_url` there too; step down only on a proven newer leader |
| 2 | medium | The kill switch clears after 200 ms, so a run still verifying targets (up to 5 s) or fetching OAuth tokens starts anyway | `src/harness/mod.rs:785`, `:1047` | a kill generation the start records and checks |
| 3 | low | A followed leader (a 409's body, `follow`) is not checked for its URL, eligibility or term | `src/harness/member.rs:610`, `:1064` | the same checks as `Membership::elected` |
| 4 | low | Node-to-node HTTP is not pinned: DNS rebinding passes `check_join_url` | `src/harness/member.rs:335`, `src/harness/safety.rs:120` | a pinned resolver with `forbidden_address` |
| 5 | low | A leader's run id becomes a file name in `RUSTYBIN_HARNESS_REPORT_DIR` unchecked | `src/harness/member.rs:849`, `src/harness/engine.rs:2524` | `safe_id` |
| 6 | low (verify first) | The verification token is public, so it proves routing, not the target's consent; the docs should say so | `src/harness/api.rs:362` | the docs, or a signed nonce |
| 7 | low | Each kill spawns a thread, and an earlier one clears a later press | `src/harness/mod.rs:785` | with 2 |
| 8 | low | A panic after the in-flight release decrements the counter twice | `src/harness/engine.rs:2897`, `:3305` | one drop guard |
