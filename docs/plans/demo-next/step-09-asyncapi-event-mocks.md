# Step 09: Event-driven mocks from AsyncAPI

## Goal

What OpenAPI mocks do for request and response APIs, for event-driven ones: load an AsyncAPI
document and Rustybin publishes valid messages on its channels, validates what clients publish, and
shows the traffic. Gateways increasingly proxy events (WebSocket, SSE, webhooks, Kafka); this gives
them an upstream to demo against.

## Where things stand

- `src/mocks/` mocks OpenAPI only: parsing (`spec.rs`), generation and validation in the OpenAPI
  schema dialect (`schema.rs`), compiled revisions, the store, captures, deploy checks.
- WebSocket, SSE and webhook demo routes exist (`websocket.rs`, `sse.rs`, `webhooks.rs`), not driven
  by a document.
- Nothing reads AsyncAPI; no broker client is in the build.

## Model

### Part A: AsyncAPI channels Rustybin serves itself

- An AsyncAPI mock: a new `kind: asyncapi` of mock in the existing store (draft, live, history, on
  and off, owner, project), so every existing mock surface and rule applies. AsyncAPI 2.6 and 3.0
  are parsed into one model: channels, operations (`send`, `receive`), messages with payload schemas
  (JSON Schema; Avro and Protobuf schemas are named in the deploy report as not generated), headers,
  examples, and the bindings Rustybin serves.
- Served bindings: `ws` (WebSocket at `/events/{name}/{channel}`), `http` with SSE (the same path,
  `Accept: text/event-stream`), and webhooks (Rustybin posts messages to a subscriber URL the caller
  registers, through `egress::Egress` under a new `Purpose::Events` with no default allowlist).
- Messages Rustybin publishes: generated from the payload schema with the existing generator (or the
  document's examples first, then stored examples, as OpenAPI mocks order them), at a rate and pattern
  per channel (`every`, `burst`, `on_receive` for request and reply), with the fault headers'
  equivalents (drop, delay, malformed).
- Messages clients publish: validated against the channel's `receive` message schema; an invalid one
  is answered with an error event and captured with the validation report.
- Captures: per channel, bounded like the mocks' capture log, `channel: "ws" | "sse" | "webhook"`.
- Deploy checks: each channel's messages generate and validate; a binding Rustybin cannot serve is a
  finding, not an error.

### Part B: Kafka, behind the cargo feature `asyncapi-kafka`

- The `kafka` binding: Rustybin produces generated messages to, and consumes and validates messages
  from, the topics of a broker the operator names (`asyncapi.kafka.brokers`, a setting; SASL
  credentials as secrets), under `Purpose::Events`. Rustybin is a client, never a broker.
- The default build gains nothing; without the feature the binding stays in the model and is refused
  at validation by name (the rule `scenario::http3_built` set).
- MQTT and AMQP: not in this step; the model leaves room and the deploy report names them.

## Surfaces

| Surface | What changes |
|---|---|
| API | `/_rustybin/mocks*` accepts AsyncAPI documents (the kind detected or given); the data plane `/events/{name}/{channel}`; `/_rustybin/mocks/{name}/subscribers` for webhooks |
| Console | The Mocks view shows an AsyncAPI mock's channels, a live message feed per channel, a composer that publishes a valid message, the deploy report |
| Management MCP | The mock tools accept AsyncAPI; `publish_event`, `list_channel_messages` (under `untrusted_*`) |
| CLI | `rustybin mock create --asyncapi <file>`, `rustybin mock events <name> <channel>` (a tail) |
| `rustybin.yaml` | AsyncAPI mocks in `mocks:` like OpenAPI ones; the `asyncapi.*` settings |

Docs: `docs/src/mocks/asyncapi.md`, an example document (an order events API) in `docs/examples/`,
Hurl examples for the HTTP parts and a small script for WebSocket and SSE.

## Owner decision (master plan section 6)

Part A first, with no new dependency; part B behind `asyncapi-kafka`; MQTT only when asked.

## Rules

- Generation and validation reuse `mocks/schema.rs`; a schema dialect difference is handled there,
  not in a second generator.
- Every channel's publishing task is bounded (one task per live channel with subscribers, stopped
  with the last subscriber or the mock), and what a revision keeps counts in
  `Compiled::memory_estimate`.
- Self-service visitors may create AsyncAPI mocks served by part A without webhooks (no outbound
  call), with the restricted responses of visitor mocks; part B is never available to them.

## Tests that fail first

- An AsyncAPI 3.0 document with a `send` and a `receive` channel: a WebSocket client receives valid
  generated messages; a published invalid message gets an error event and a capture with the report.
- The same document in 2.6 form compiles to the same channels.
- SSE on the same channel gets the same messages.
- A webhook subscriber outside the allowlist is refused before any call.
- With `asyncapi-kafka`: against a broker started by the test (or skipped with a message when none is
  configured, as `quota-postgres` does), messages produced and consumed; without the feature, a
  `kafka` binding is refused by name.

## Done when

An AsyncAPI document becomes a working event mock on every surface (part A), Kafka works behind its
feature (part B, or its row says no), documented with tested examples.

## Build notes (2026-11-12)

Part A is built whole, with the recommendation of section 6. Part B (Kafka) is not built: the
owner chose part A now and a decision on Kafka after seeing it. Where the build differs from the
plan above:

- **An AsyncAPI mock is a `Compiled` revision with channels and no operations**:
  `ParsedSpec.events` (`mocks::asyncapi::EventSpec`), filled by `spec::parse_value` when the
  document has an `asyncapi` key. Nothing else in the store changed, so drafts, deploys,
  history, owners, projects, persistence, exports, checkpoints and packs carry such a mock as
  they carry any other. The kind is detected, never given: there is no `kind` field to set and
  no `--asyncapi` flag (`rustybin mocks create --spec` takes either document). A mock's summary
  and detail answer `kind` (`openapi` or `asyncapi`).
- **A channel's id is its address as a slug** (`orders/created` is `orders-created`, numbered on
  a clash), in 2.x and 3.0 alike, so both forms of a document give the same channels.
- **Every channel is served all three ways** (WebSocket, SSE, webhooks) whatever bindings the
  document names; a binding or server protocol that is not `ws` or `http` is one warning of the
  deploy report. A `POST` to the channel publishes as a client (the `http` binding's other
  half), and a plain `GET` describes the channel.
- **The data plane also answers below the mock's root**, at
  `/mock/{name}/_rustybin/events/{channel}`: a visitor's mock answers only on the mocks domain,
  whose host rewrite leads to `/mock/{name}`. `/events/{name}/{channel}` refuses a visitor's
  mock when the instance has a mocks domain, as `/mock/{name}` does.
- **Patterns**: `every`, `burst`, `on_receive` and also `off` (only what is published by hand).
  The default is `every`, `on_receive` on a channel whose document declares a reply, `off` on a
  channel that only receives. A 3.0 `reply` puts the reply's messages among what the reply's
  channel sends; 2.x has no reply.
- **Faults are settings of the channel** (`delay_ms`, `drop_percent`, `malformed_percent`),
  drawn from the mock's seed so a run repeats; there are no per-connection fault parameters. A
  message published by hand is never spoiled.
- **One more control route than the plan's**: `GET /_rustybin/mocks/{name}/channels` (the
  channels with their settings, a valid sample and live numbers), beside
  `.../channels/{channel}/messages`, `.../channels/{channel}/publish` and the subscribers
  routes. Its tool is `list_channels`; the subscribers have `list_event_subscribers`,
  `add_event_subscriber` (confirmed) and `remove_event_subscriber`. Six tools, 169 in all.
- **No control-plane stream of messages**: the console's feed and `rustybin mocks events
  --follow` ask the list again with `after`. A never-ending control stream must check its
  caller again at every event (`handlers::still_readable`); a short poll gets that check for
  free.
- **Messages live in the mock's capture log** (`CaptureLog::record_event`, `EventRecord`), not
  in a store of their own: bounded by the mock's capture limits, cleared with its requests,
  gone with the mock. The record's `channel` is the channel's id, so how a received message
  arrived is `transport` (`ws` or `http`), and a sent message has `delivered` (how many
  WebSocket, SSE and webhook subscribers it went to) in place of one record per transport.
  `TrafficScope::sees_event`: a caller who reads only their own traffic sees what Rustybin
  published and what they published.
- **A hub belongs to its mock** (`Hub.log`, compared with `CaptureLog::same`): a mock deleted
  and made again under the same name starts its channels at message one and lets the old
  mock's listeners go. Found while running the docs examples twice against one instance.
- **Webhook subscribers are in memory only**: an hour by default, at most a day, removed after
  ten failed deliveries in a row, 10 a mock and `asyncapi.max_subscribers` an instance, never in
  a document, an export or a checkpoint. A URL's query is never answered again. The host is
  checked with `Egress::dry_check` when the subscriber is added and resolved again at every
  delivery; at most 16 deliveries are under way at once.
- **Bounds beyond the plan's**: `asyncapi.max_connections` (128) and `asyncapi.session_minutes`
  (60) as settings, 1024 hubs, 50 messages a second and 256 KiB a message on one connection, 50
  messages and 20 examples of a message kept at parse.
- **Security settings do not apply to channels** (a warning of the deploy report says so), and
  the parts of a mock built on operations (its MCP server's tools, stateful data, drift,
  collections, the portal's try-it) have nothing to work on for an event mock. The portal still
  lists such a mock as a candidate; publishing one shows an API with no operations.
- **Self-service visitors** may create event mocks; their HTTP answers carry the visitor
  response headers (an SSE stream keeps its media type), and webhook subscribers are refused
  for them twice: the routes are not self-service, and `events::subscribe` refuses a visitor's
  mock.
- The tests were written with the code: `src/events/tests.rs` (the plan's first four, plus the
  reply, the faults, the task's end, the connection cap, an allowed webhook on loopback, the
  deploy report, the settings in a document and a mock made again) against a real WebSocket
  server, and one in `src/mcp/manage/tests.rs` for the tools.
  `docs/examples/mocks/asyncapi.hurl` and `asyncapi-stream.sh` (websocat) run the examples.

### For the merge

`src/mocks/asyncapi.rs`, `src/events/` (`mod.rs`, `serve.rs`), `ParsedSpec.events`,
`MockSettings.channels` with `ChannelSettings` (and its place in `MockDoc`, `CreateBody`,
`PatchBody`, the config schema and the configdoc diff), `Report.channels`, the event log inside
`CaptureLog`, `Purpose::Events`, `AppState.events`, `events::specs` and the settings group
`asyncapi`, `src/mcp/manage/events.rs`, six `mocks` commands, `ui/views/mocks/events.js`.

### Part B, if the owner says go

Kafka as a client of a broker the operator names, behind the cargo feature `asyncapi-kafka`:
the model already keeps each channel's bindings (`Channel.bindings`), the deploy report already
names `kafka` as not served, and a publish already goes through one function
(`Events::publish`), which is where a producer would be handed the frame beside the webhooks.
