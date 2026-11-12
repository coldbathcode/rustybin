# Event mocks from AsyncAPI

What an OpenAPI mock does for a request and response API, an **event mock** does for an
event-driven one. Give Rustybin an AsyncAPI document and it serves the document's channels:
it publishes valid messages on them, validates what clients publish, and shows the traffic.
A gateway that proxies WebSockets, server-sent events or webhooks gets an upstream to demo
against.

An event mock is a mock like any other: the same list, drafts and deploys, revisions,
projects, exports and `rustybin.yaml`. It has **channels** where an OpenAPI mock has
operations. Create one the way you create any mock, with an AsyncAPI 2.x or 3.0 document
(JSON or YAML) in place of the OpenAPI one; the kind is detected.

```yaml
{{#include ../../examples/mocks/order-events.yaml}}
```

```hurl
{{#include ../../examples/mocks/asyncapi.hurl:apply}}
```

## What Rustybin plays

An AsyncAPI document describes an application. Rustybin plays that application:

| In the document | Rustybin |
|---|---|
| A `send` operation (3.0), or a `subscribe` one (2.x: what a client may subscribe to) | Publishes the operation's messages on the channel |
| A `receive` operation (3.0), or a `publish` one (2.x) | Accepts those messages from clients and validates them |
| A `reply` on a `receive` operation (3.0) | Answers each valid message with the reply's message |

Both versions become the same channels. A channel's **id** is its address as a slug
(`orders/created` is `orders-created`); it names the channel in URLs, settings and commands.

```hurl
{{#include ../../examples/mocks/asyncapi.hurl:channels}}
```

## Connecting to a channel

Each channel answers at `/events/{mock}/{channel}`, three ways:

| How | What you get |
|---|---|
| A WebSocket | Each published message as a text frame (the payload); send a frame to publish as a client |
| `GET` with `Accept: text/event-stream` | Server-sent events: `event` is the message's name, `id` its number on the channel, `data` the payload |
| `POST` a message | Published as a client: `202` when valid, `422` with the issues |

A plain `GET` describes the channel. The same channel also answers below the mock's own
root, at `/mock/{mock}/_rustybin/events/{channel}`, which is where a
[self-service](self-service.md) mock on its own domain serves it.

```sh
{{#include ../../examples/mocks/asyncapi-stream.sh:websocket}}
```

```sh
{{#include ../../examples/mocks/asyncapi-stream.sh:sse}}
```

```hurl
{{#include ../../examples/mocks/asyncapi.hurl:describe}}
```

Every subscriber of a channel gets the same messages, whichever way it listens. A channel
publishes only while somebody listens (a connection or a webhook subscriber), and a
connection is closed after an hour (the `asyncapi.session_minutes` setting). When the mock
is deleted or turned off, its connections are closed.

## What is published

A channel's messages are published in turn. Each payload comes from, in order:

1. the message's `examples` in the document, in turn;
2. the examples stored with the mock (`channels.<id>.examples`), in turn;
3. the payload schema, through the generator of the OpenAPI mocks (the mock's
   `responses.seed` decides the values: the same seed, the same messages).

**When** they are published is the channel's `pattern`:

| `pattern` | Publishes |
|---|---|
| `every` | One message every `every_ms` (1000 by default, at least 100). The default |
| `burst` | `burst` messages (5 by default, at most 20) at once every `every_ms` |
| `on_receive` | One message for each valid message a client publishes: request and reply. The default on a channel whose document declares a `reply` |
| `off` | Nothing by itself: only what you publish by hand. The default on a channel that only receives |

Publish by hand at any time, on any pattern: the channel's next message, several, or a
payload of your own.

```hurl
{{#include ../../examples/mocks/asyncapi.hurl:publish}}
```

```hurl
{{#include ../../examples/mocks/asyncapi.hurl:publish_given}}
```

### Faults

A channel's settings can spoil what the pattern publishes, to show what a gateway or a
client does about it:

| Setting | Effect |
|---|---|
| `delay_ms` | Waited before each message is published |
| `drop_percent` | That share of messages reaches nobody (the log marks them `dropped`) |
| `malformed_percent` | That share is sent cut short, so it is not valid JSON (`malformed`) |

Which messages are hit follows from the seed, so a run repeats. A message you publish by
hand is never spoiled.

## What clients publish

A message a client publishes is checked against the channel's `receive` messages: it is
valid when it meets the payload schema of one of them. A valid message is accepted without
an answer (or with the reply, on `on_receive`); an invalid one is answered with an error,
and both are kept with the verdict.

```hurl
{{#include ../../examples/mocks/asyncapi.hurl:client_valid}}
```

```hurl
{{#include ../../examples/mocks/asyncapi.hurl:client_invalid}}
```

Over a WebSocket the error is an event on the same connection:

```sh
{{#include ../../examples/mocks/asyncapi-stream.sh:invalid}}
```

A channel whose document declares a reply answers a valid request:

```hurl
{{#include ../../examples/mocks/asyncapi.hurl:reply}}
```

```sh
{{#include ../../examples/mocks/asyncapi-stream.sh:reply}}
```

One connection may publish 50 messages a second, each of at most 256 KiB.

## The messages of a channel

Everything published and received is kept in the mock's log, bounded like its requests
(the oldest go first) and cleared with them. A message's payload is kept up to 8 KiB.

```hurl
{{#include ../../examples/mocks/asyncapi.hurl:messages}}
```

`after` asks for what came after a sequence number, which is how the console's feed and
`rustybin mocks events --follow` keep up:

```hurl
{{#include ../../examples/mocks/asyncapi.hurl:after}}
```

On a public instance a caller without a credential reads what Rustybin published and what
they published themselves, never another caller's messages, as with a mock's
[requests](index.md).

## Webhook subscribers

A webhook subscriber is a URL every message of a channel is posted to: the payload as the
body, with `X-Rustybin-Event` (the message's name), `X-Rustybin-Event-Id`,
`X-Rustybin-Mock` and `X-Rustybin-Channel`, plus the headers the document gives the
message.

Posting to a URL reaches out, so it follows the rules of everything that does: it needs
`mocks.write`, and goes through the [outbound policy](../concepts/control-plane-security.md)
under the purpose `events`, which has no default. Add the receiver's host to
`RUSTYBIN_EGRESS_ALLOW`; a host that is not allowed is refused when the subscriber is
added, before anything is sent:

```hurl
{{#include ../../examples/mocks/asyncapi.hurl:subscribe_refused}}
```

```hurl
{{#include ../../examples/mocks/asyncapi.hurl:subscribers}}
```

```hurl
{{#include ../../examples/mocks/asyncapi.hurl:unsubscribe}}
```

A subscriber lives an hour by default (`ttl_seconds`, at most a day), is removed after ten
failed deliveries in a row, and is never part of a document or an export: it lives in
memory and is gone at a restart. A URL's query is never shown again (it may hold the
receiver's credential). A mock has at most 10 subscribers, the instance 50 (the
`asyncapi.max_subscribers` setting). Self-service mocks have none.

## The deploy report

The deploy checks build each channel's messages once and validate them against their own
schemas. What Rustybin cannot serve or generate is a warning, never an error, so the mock
still deploys:

| Finding | When |
|---|---|
| A binding or protocol that is not served | The document names `kafka`, `mqtt`, `amqp` or another binding: the channels are served over WebSocket, server-sent events and webhooks all the same |
| A schema format that is not generated | A message's `schemaFormat` is Avro, Protobuf or another format: give the message an example, which is published as it is |
| An example that does not match | A document or stored example breaks the message's own schema |
| No payload schema and no example | An empty object is published |
| Security settings | They do not apply to channels, which answer without a credential |

```hurl
{{#include ../../examples/mocks/asyncapi.hurl:report}}
```

The routes of this page are for event mocks only:

```hurl
{{#include ../../examples/mocks/asyncapi.hurl:not_asyncapi}}
```

## Limits

| Limit | Value |
|---|---|
| Connections to channels at once | 128 (the `asyncapi.max_connections` setting, 1 to 1024); one more is refused with `429` |
| Longest connection | 60 minutes (`asyncapi.session_minutes`) |
| A message a client publishes | 256 KiB; 50 a second on one connection |
| Channels of a document | The mock's operation limit (500) |
| Webhook subscribers | 10 a mock, 50 an instance (`asyncapi.max_subscribers`); a delivery waits 5 seconds |

An event mock has no operations, so the parts of a mock built on them do not apply to it:
its MCP server has no tools, and mocked security, stateful data, drift checks and
collections have nothing to work on.

## On every surface

| Surface | Where |
|---|---|
| API | A mock's create, `PATCH` and apply take an AsyncAPI document and `channels`; `/events/{name}/{channel}` (WebSocket, SSE, `POST`); `GET /_rustybin/mocks/{name}/channels`, `GET .../channels/{channel}/messages`, `POST .../channels/{channel}/publish`; `GET` and `POST /_rustybin/mocks/{name}/subscribers`, `DELETE .../subscribers/{id}` |
| Console | **Mock APIs**: drop an AsyncAPI file. The mock's **Channels** tab: each channel's URL, what it publishes and accepts, its pattern and faults (**Save to the draft**), **Publish the next message**, a composer that publishes a valid message as a client, the live feed of messages, and the webhook subscribers. **Deploy report** lists the channels. Management, General has the caps under **Event mocks** |
| CLI | `rustybin mocks create <name> --spec <asyncapi file>`, `rustybin mocks channels <name>`, `rustybin mocks events <name> <channel> [--follow]`, `rustybin mocks publish <name> <channel> [--data JSON] [--count N]`, `rustybin mocks subscribers <name>`, `rustybin mocks subscribe <name> <channel> --webhook URL`, `rustybin mocks unsubscribe <name> <id>` ([reference](../cli.md#mocks)) |
| Management MCP | `create_mock` and `update_mock` take an AsyncAPI document and `channels`; `list_channels`, `list_channel_messages` (payloads under `untrusted_*`), `publish_event`, `list_event_subscribers`, `add_event_subscriber` (confirmed: it sends messages out), `remove_event_subscriber` |
| `rustybin.yaml` | An AsyncAPI document as a mock's `spec` or `spec_inline`, its `channels:`; `settings:` with `asyncapi.max_connections`, `asyncapi.session_minutes` and `asyncapi.max_subscribers` |
