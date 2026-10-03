# WebSocket

| Route | Behaviour |
|---|---|
| `GET /ws` | Echoes every text and binary frame back |
| `GET /ws/time?interval_ms=&count=` | Sends `{"tick": n, "timestamp": ...}` every `interval_ms` (default 1000, 100 to 60000) for `count` ticks (default 10, 1 to 1000), then closes normally |
| `GET /graphql/ws` | [GraphQL subscriptions](graphql.md#subscriptions) |

Common behaviour, useful for gateway WebSocket proxying tests:

- **Subprotocols**: the first protocol offered in `Sec-WebSocket-Protocol` is
  echoed back, so subprotocol pass-through can be checked.
- **Ping / pong** is answered by the WebSocket stack.
- **Close handshake**: a client close is answered with a close frame; the ticker
  keeps reading the socket, so a client close or ping mid-stream is honoured.
- **Limits**: messages up to 1 MiB (256 KiB in public mode, else close `1009`); idle
  timeout 5 minutes (1 minute) and maximum lifetime 1 hour (10 minutes), both closing
  with `1001`.

Examples with [websocat](https://github.com/vi/websocat) (`$WS` is the base URL
with `ws://`, for example `ws://localhost:8080`; use `wss://` and port 443 for the
HTTPS listener):

```bash
{{#include ../../examples/protocols/websocket.sh:ws_echo}}
```

```bash
{{#include ../../examples/protocols/websocket.sh:ws_ticker}}
```

The upgrade handshake with plain curl, showing the echoed subprotocol:

```bash
{{#include ../../examples/protocols/websocket.sh:ws_subprotocol}}
```

The WebSocket routes are captured by the request inspector (the upgrade request)
and honour `X-Rustybin-Delay` / `X-Rustybin-Fail` on the handshake like any other
route.
