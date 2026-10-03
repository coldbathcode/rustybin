# Server-Sent Events

Generic event streams for testing how a gateway proxies `text/event-stream`:
buffering, timeouts, reconnection with `Last-Event-ID` and response
transformation of streams. (The mock LLM has its own provider-native streams, see
[Providers](../ai/providers.md).)

## Numbered events: `/sse`

`GET /sse` sends numbered events with `id: n`, `data:` and, optionally, an `event:`
name. Query parameters:

| Parameter | Default | Meaning |
|---|---|---|
| `count` | 10 | Number of events (max 1000; 100 in public mode) |
| `interval_ms` | 1000 | Time between events (max 60000; 10000 in public mode) |
| `event` | none | Event name (`event:` line) |
| `retry` | none | Reconnection delay (`retry:` line) sent with the first event |
| `mode` | `json` | `json` (`{"id", "count", "message", "timestamp"}`) or `text` |
| `heartbeat_ms` | 15000 | Comment heartbeat (`: heartbeat`) while idle; `0` disables |
| `last_event_id` | none | Same as the `Last-Event-ID` header |

Event ids are sequence numbers: a client that reconnects with `Last-Event-ID: 3`
resumes at event 4. Streams end after `count` events, or after 10 minutes (2 in
public mode). Responses carry `Cache-Control: no-cache` and
`X-Accel-Buffering: no` so well-behaved proxies do not buffer them.

```hurl
{{#include ../../examples/protocols/sse.hurl:sse}}
```

```hurl
{{#include ../../examples/protocols/sse.hurl:sse_resume}}
```

```hurl
{{#include ../../examples/protocols/sse.hurl:sse_text}}
```

## LLM-style text stream: `/sse/chat`

`GET /sse/chat?prompt=&max_tokens=&delay_ms=` or `POST /sse/chat` with
`{"prompt", "max_tokens", "delay_ms"}` streams a reply one word per event, in a
provider-neutral shape:

```text
data: {"id":"chat-...","object":"chat.chunk","index":0,"delta":"You ","finish_reason":null}
...
data: {"id":"chat-...","object":"chat.chunk","index":5,"delta":"","finish_reason":"length","usage":{...}}

data: [DONE]
```

`delay_ms` defaults to 50 (max 2000; 1000 in public mode) and `max_tokens` to 500
(max 200 in public mode). The last chunk carries `finish_reason` (`stop` or `length`)
and `usage`.

```hurl
{{#include ../../examples/protocols/sse.hurl:sse_chat}}
```

Other server-sent event streams in Rustybin: the
[inspector feed](../concepts/inspector.md#live-feed), the
[request bin feed](request-bin.md), the MCP Streamable HTTP and legacy HTTP+SSE
transports, A2A streaming, and every mock LLM stream.
