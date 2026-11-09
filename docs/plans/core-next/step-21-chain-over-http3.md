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
