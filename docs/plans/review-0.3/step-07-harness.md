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

## Build notes (2026-11-02)

Every finding has a test that failed on `10d88f9` (finding 6 excepted, see there) and passes
now. Files touched: `src/harness/` (api, cluster, election, engine, member, mod, safety and
the tests) and `docs/src/harness.md`; nothing outside the step's area.

1. **Leadership.** A heartbeat with a newer term no longer marks the leader stale: it answers
   `409` to that member and records the claim (`Cluster::take_newer_claim`, only within
   `MAX_TERM_JUMP`). The leader's ticker (`election::lead`) then probes the eligible nodes,
   at most every half election timeout, and steps down only on a proven newer leader:
   `Cluster::check_new_leader` (an eligible member other than itself, at most
   `MAX_TERM_JUMP` ahead, at the control URL it joined with, which fills in a missing one).
   A direct answer (the peer called at its own URL answers as leader, node id matching) is
   exempt from the term bound, so a leader partitioned through many elections can still
   step down. The leader branch of `node_elected` runs the same check plus
   `check_join_url` before it steps down. Tests: `a_member_cannot_depose_the_leader`
   (failover, over HTTP: an ineligible member's claim and announcement, an eligible node's
   wrong URL and huge term; the leader still leads after its probe) and the updated
   `a_snapshot_carries_what_a_successor_needs` (claim recorded, not stale; every refusal of
   `check_new_leader`).
2. **Kill during preparation.** `KillSwitch` counts a generation per press;
   `start_local_run` records it before its first await and refuses to start when it
   changed, even after the switch cleared. Test:
   `a_kill_during_a_long_verification_prevents_the_start` (a 1 s verification, the switch
   cleared at 350 ms).
3. **Followed leaders.** `Membership::follow` now runs `checked_leader` (the snapshot's
   eligible list, `MAX_TERM_JUMP`, the control URL from the succession, the members or the
   snapshot's leader) and returns whether it followed; `elected` uses the same function.
   This covers a 409's leader in the agent loop (a refused one counts as no answer, no
   tight loop), `probe_next`, `stand` and `step_down`. A direct answer in `probe_next`
   must come from the node the succession lists at that URL. A node with no snapshot yet
   (just joined) has nothing to check against and follows; the URL checks of finding 4
   still apply to every call. Test: the `follow` part of
   `a_member_cannot_depose_the_leader`.
4. **Node calls pinned.** `safety::NodeResolver` is the node client's resolver: every
   connection resolves through it and is refused when any address is forbidden; the
   client also has `no_proxy()`. `post_json` runs `safety::check_node_url` first (scheme,
   no credentials, a literal address never forbidden: literals never reach a resolver).
   `check_join_url` is that plus a resolution. Chosen over pinning per call because the
   node client is one long-lived client for many peers; refusing at each connection closes
   the rebinding window the same way. Tests: `node_calls_never_reach_forbidden_addresses`
   (member) and `node_resolver_refuses_forbidden_addresses` (safety).
5. **Run ids as file names.** `start_local_run` refuses an `options.id` that is not
   `cluster::safe_id` (400), and both report writers (`engine`, and the cluster's, whose id
   may come from a snapshot) write only a safe id. Test: `an_unsafe_run_id_is_refused`.
6. **Verification token (verify first): closed with docs, no code change.**
   `verification_proves_routing_not_consent` passes on today's code: the token is read
   without a credential and a server that copies it passes verification. A signed nonce
   would not prove consent either (a relay to a node answers any challenge), so
   `docs/src/harness.md` now says exactly what verification proves (the target routes the
   path to a node serving the token, and the route works) and what it does not (the
   owner's consent; that comes from the allowlist and the caps).
7. **One clearer per kill.** `KillSwitch::press_for` starts at most one clearing thread;
   it clears only after a whole hold with no new press, so an earlier press never clears
   a later one, and repeated presses spawn no threads. Test:
   `a_later_kill_is_not_cleared_by_an_earlier_one`.
8. **In-flight released once.** `engine::InFlight` is the request's slot: `release()` is
   idempotent and `Drop` gives it back if the request did not, so the `catch_unwind` in
   `send_one` only logs. Test: `a_panic_after_the_release_gives_the_slot_back_once`
   (a test-only hook, `engine::PANIC_AFTER_RELEASE`, panics after the release; on the old
   code the count wrapped to `usize::MAX` and half the requests were not sent).
