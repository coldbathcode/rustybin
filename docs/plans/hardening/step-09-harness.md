# Step 09: Harness: one run at a time, cluster leadership, panics

## Goal

The node's caps and the kill switch hold under concurrent requests and after re-splits; only an
eligible node can lead; no answer from a target can panic the engine.

## Findings

| Sev | Where | Problem | Fix |
|---|---|---|---|
| high | `src/harness/mod.rs:556-573,716-740` | `start_local_run` checks the kill switch and the active run, then awaits DNS, OAuth tokens and verification before inserting the run with no second check: N parallel starts all run (N times `max_rate`); a run still verifying when kill is pressed starts after the switch clears (200 ms) | reserve the slot before the first await (a placeholder or a `creating` flag under the same mutex, released on error); re-check `kill.is_set()` right before `engine::start` |
| medium | `src/harness/api.rs:1028-1037`, `src/harness/member.rs:1030-1043,1143-1159,1235-1240,191,776` | an `elected` announcement is accepted from any node with a valid credential and a newer term: no eligibility, vote or plausibility check; `follow()` skips `check_join_url`; a term of `u64::MAX` wraps `term + 1` (debug panic, failover broken); `post_json` reads bodies unbounded; `lease_ms` unbounded | accept `elected` only from `snap.eligible` with `term <= current + small bound` and the `control_url` that node joined with; `check_join_url` in `follow`; `Limited` bodies; `checked_add`; bound the lease |
| medium | `src/harness/member.rs:915-918`, `engine.rs:3730` | a re-split share is validated without live multipliers and the scheduler never clamps: multiplier 2 at share 0.5, then share 1.0, sends twice `max_rate` | `run.controls().check(&run.scenario, &h.caps(), share.fraction())`; clamp in the scheduler too |
| medium | `src/harness/engine.rs:3142,3164-3167` | `Duration::from_secs_f64` on `Server-Timing` `dur=1e300` or `inf`, and on `Retry-After: 1e20`, panics; the in-flight counter is never decremented, so repeated panics fill `inflight_cap` and the run sends nothing | `Duration::try_from_secs_f64(..).ok()` after clamping; decrement in-flight in a drop guard |
| low | `src/harness/scenario.rs:1822`, `engine.rs:1880` | the control-path guard is a prefix check on the raw path (`/api/../_rustybin/x` passes) and `{{follow}}` is inserted raw from the target's answer | normalise dot segments before `is_control_prefix`; percent-encode the follow id as a segment |
| low | `src/harness/identity.rs:178-192` | a failing OAuth token endpoint is fetched once per request | cache the failure for a short backoff |

The join token: a holder can already join as an eligible node and receive the cluster secret. Owner
decision (master plan section 6): document it, or add operator approval for eligible nodes.

## Tests

Ten parallel run creations produce one run and nine 409s; kill during verification prevents the
start; a forged `elected` from an ineligible node or with a huge term is refused; a re-split with a
live multiplier is refused; the test proxy answering `Server-Timing: rb;dur=1e300` and
`Retry-After: 1e20` leaves the run healthy and the in-flight count at zero after drain.

## Done when

The tests pass and the conformance cluster run (`conformance/harness/run.sh`) is green.
