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

## Build notes (2026-11-02)

- Every finding is fixed; none was closed as not reproducible. Section 6's decision:
  the join token is documented as a cluster credential (no approval step).
- `start_local_run` takes a `StartSlot` (an atomic flag, released on drop) right after
  the cheap checks and before the first await, checks the active run again, and checks
  the kill switch once more just before the run is listed and started.
- `Membership::elected` refuses a leader that is not in `snapshot.eligible` (403), a
  term more than `MAX_TERM_JUMP` (16) ahead (409), and a control URL other than the
  one the snapshot's succession records; the API handler runs `check_join_url` on the
  announced URL first. The candidate's term is `saturating_add(1)`, peer answers are
  read through `read_bounded` (8 MiB), lease, heartbeat and election timeout are
  clamped to 10 minutes.
- A re-split share is checked with `controls().check(..)` as well as
  `validate_scaled`; the scheduler clamps each layer's rate to the node's
  `max_rate`.
- `engine::secs` turns any float into a `Duration` (NaN and negatives 0, at most a
  day); every `from_secs_f64` in the engine goes through it, and `Retry-After` waits
  at most an hour. `send_one` wraps the request in `catch_unwind` and gives back its
  in-flight slot (saturating) if it panicked. The plan's proxy test is covered by the
  unit test of `secs` (the proxy cannot send a tag without the run key), plus the
  guard.
- `egress::normalise_path` (lowercase, `%5f %2e %2f %5c` and backslashes decoded, dot
  segments resolved) backs `is_control_prefix`; `{{follow}}` goes into a path as one
  percent-encoded segment (dots encoded too).
- `OAuthPool` remembers a failed token request for 2 s per client.
- `conformance/harness/run.sh` passed (leader killed at 10 s, the standby led term 2
  after 1.8 s, nothing lost).
