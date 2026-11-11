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
