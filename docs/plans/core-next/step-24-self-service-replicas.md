# Step 24: Self-service across replicas

Waits for the owner (section 6). Recommended: not until one instance is measurably full.

## What it would be

Visits and visitor mocks live in memory on one instance (saved to the state directory, so a restart
keeps them). A hosted demo with more than one replica needs every replica to know every visit and
mock, or every request for a mock routed to the replica that holds it.

## Why it waits

The demo is one instance by the owner's decision of 2026-10-18, it has no visitors today, and
nothing has measured it full. This is the largest item left and the only one that needs a second
running instance to prove.

## The choices

- **A shared store** (recommended if built): visits, the daily ledger and visitor mocks in
  PostgreSQL behind a cargo feature, the way `quota-postgres` holds the quota counters
  (`keys::store::QuotaStore`).
- **Routing by mock host at the proxy**: no code in this repository for the data plane, but the
  control plane calls of a visit must reach the same replica too, and a replica that goes away takes
  its visits with it.
- **Stay on one instance** and raise its limits.

## What a shared store has to settle

- **Captured traffic.** The hosted mocks plan says captures are never persisted. Visitors watch
  their captures, so with replicas either captures go to the store (a change of that rule, with a
  short life and the visit's isolation kept: `TrafficScope`, `CaptureLog::visible`), or the data
  plane and the capture reads of one mock are routed to one replica.
- **Compiled revisions.** Each replica compiles on first use and bounds what it keeps
  (`Compiled::memory_estimate`, `MockStore::check_memory`); the visitors' memory budget becomes per
  replica, and a deploy on one replica must be seen by the others before they serve it.
- **Limits.** The creation rate, the visit clock and the per-address limits are counted once for
  the instance, not once per replica.
- **Stateful mode** for visitor mocks stays off across replicas, or its data goes to the store too.
- **Email leads** keep leaving through the queue only, never the store.

## How it would be built

A spike first, as the harness did (phase 0): two instances and a database in a compose project, a
conformance run that creates a visit on one and calls its mock on the other, with one replica
killed. The spike's findings become the step's design; no production code before it.

## Tests that would fail first

A visit made on one replica is known on the other; a mock created on one answers on the other; the
creation cap holds across both; a visitor never reads another's captures through either; a replica
killed loses no visit.

## Done when

The owner says build it and the compose run passes, or this row keeps saying it waits.
