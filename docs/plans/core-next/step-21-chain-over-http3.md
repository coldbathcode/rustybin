# Step 21: A chain hop over HTTP/3

## Goal

Step 13 built HTTP/3 targets and server-to-server chains and refuses the two together: a hop with
`http: h3` fails validation (`chain.rs`). A mesh whose inner hops speak HTTP/3 is tested end to end.

## Design

- Only in a build with the cargo feature `harness-http3`; a build without it keeps refusing the
  hop by the feature's name (`scenario::http3_built`).
- The onward call of an `h3` hop is made by the client of `http3.rs`, handed the addresses
  `safety::pin_chain` pinned. It resolves nothing itself: no `Alt-Svc`, no preferred address, as for
  a target.
- Everything else about a chain is unchanged: the gate (`safety::ChainGate`), one bodiless `GET`,
  the hop count inside the marker's signature, the depth, the report's legs.
- A hop is verified over HTTP/3 when it is called over HTTP/3, as a target is.
- QUIC connections per hop are bounded like a target's, and closed when the run ends or the kill
  switch is set.
- Mixed chains are allowed: a client target over HTTP/1 or 2 with an `h3` hop, and the reverse.

## Surfaces

The schema already has `http` on a hop; validation stops refusing it under the feature. The console
form, the MCP tool schemas and the CLI follow the schema. Docs: the HTTP/3 and chain sections of
`docs/src/harness.md` lose the "refused" sentence and gain an example under the feature.

## Tests that fail first (under the feature)

- A chain whose hop is an in-process QUIC server on loopback: the call arrives, the leg is in the
  report.
- The hop's server asks the client to move to a preferred address: nothing is sent there.
- The kill switch ends a call under way and closes the connections.
- Without the feature: the hop is refused at validation with the feature's name.

## Done when

`cargo test --features harness-http3` covers a chain over HTTP/3, the default build is unchanged
(`cargo tree` shows no QUIC crate), and the docs example runs.

## Build notes

Built 2026-11-12. Where the build is more exact than the plan, or differs:

- **No new client.** A hop was already built as the engine's `PinnedTarget`, which has carried the
  HTTP/3 client of an `h3` target since step 13. So the step is small: `Chain::prepare` no longer
  refuses the hop, `Chain::send` hands the same request (marker and all) to `target.h3` when there
  is one, and the hop is verified over HTTP/3 by the `verify_target` a target uses. The gate, the
  marker, `ChainStats` and the series of step 16 are untouched: an `h3` call is counted through
  `call_on` like any other, and there is no new outcome or `ChainRefusal`.
- **The schema did not have `h3` on a hop.** The plan says it had; the enum of a hop's `http` was
  `auto`, `h1`, `h2`, `h2c`. It gained `h3` in every build (the description names the feature), so
  the console form and the tools' schemas show it everywhere and validation refuses it by name
  without the feature (`servers.chain.hops[i].http: h3 needs a build with the cargo feature
  harness-http3 ...`), the same words as a target's. An `h3` hop on a plain `http` URL is refused in
  every build.
- **Closing the connections needed an owner.** A target's QUIC connections go with the run's
  targets. A chain is kept on the run's entry after the run (for the report), so nothing closed a
  hop's. `H3Client` gained `close_connections` (every connection now, a new one on the next use)
  and `shut` (for good: no connection again, the endpoints closed). `RunEntry::chain_not_live`
  closes (a cluster's leader may say live again), `RunEntry::finish` shuts, and a task per chain
  with an `h3` hop (`chain::close_quic_on_kill`, ended when the chain is shut or dropped) closes
  them while the kill switch is set, every 50 ms, so a call that was on its way cannot leave one
  open. A slot of the client is now a lock for connecting plus a lock held only for a moment, and
  a connection made while a close happened is never kept (an epoch checked under the lock), so a
  close never waits for a handshake.
- **A call stopped by a close is `stopped`.** Closing a connection under a call can make the call
  see its stream reset before it sees the kill switch or the run's end. For an `h3` hop such a
  transport outcome is counted as the stop it is (`kill_switch` or `not_live`), as a call over
  HTTP/1 or 2 is, never as `failed`.
- **An answer past the bound.** A chain reads at most 1 MiB of an answer and lets the rest go, and
  the call still counts as answered. `H3Client::request_bounded` does the same over HTTP/3 (the
  stream is stopped); a target's `request` keeps failing an answer over 16 MiB as before.
- **Tests** are in `src/harness/http3_tests.rs` (the in-process QUIC gateway there now counts
  connections opened and closed), plus `an_h3_hop_needs_the_feature` in `chain_tests.rs` and
  `an_h3_hop_is_in_the_schema_and_refused_without_the_feature` in `scenario.rs`, which run in both
  builds. They were written with the code; each protection was then removed once to see its test
  fail: the call not sent over QUIC, no kill switch task, the run's end closing nothing, the run's
  end not shutting, a stopped call not counted as stopped. The preferred address test passes
  because the QUIC stack never moves a client; there is no line of ours to remove, as for a
  target's.
- **One existing assertion changed**: the row of `scenario.rs`'s chain validation table that
  expected an `h3` hop to be refused ("the server role has no HTTP/3 client") is now the refusal of
  an `h3` hop on a plain URL, since the old one is what this step removes.

Not built: nothing of the plan. Not in the plan and not built: the protocol of a hop in the
report's leg, HTTP/3 for the node-to-node calls of a cluster.

## For the merge

Changelog (Unreleased, harness):

- Load test mode: a hop of a [server-to-server chain](harness.md#a-hop-over-http3) takes
  `http: h3` in builds with the cargo feature `harness-http3`, so a mesh whose inner hops speak
  HTTP/3 is tested end to end. The hop is pinned, verified over HTTP/3 and called on its pinned
  addresses only; its QUIC connections are closed when the kill switch is set or the run ends.
  Chains may mix protocols. A build without the feature still refuses the hop at validation, now
  with a message that names the feature.

Rule for CLAUDE.md (core next, harness chains): an `h3` hop is called by the `H3Client` of the
hop's `PinnedTarget` and by nothing else, counted through `chain::call_on` like every onward call;
what holds QUIC connections outside a run's targets closes them itself
(`Chain::close_connections` from `RunEntry::chain_not_live`, `Chain::shut` from
`RunEntry::finish`, `chain::close_quic_on_kill` while the kill switch is set), and a call ended by
such a close is counted as stopped, not failed.

Upgrade note: in a cluster the leader validates and the server nodes call. A leader with the
feature and a server node without it (or one built before this step): that node cannot prepare
the chain, makes no onward call and says why in `server.chain.problems` (`not_prepared` in the
report and the series). A leader without the feature refuses the scenario.

Roadmap: "a chain hop over HTTP/3" is done.
