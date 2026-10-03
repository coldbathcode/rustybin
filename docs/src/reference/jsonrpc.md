# JSON-RPC

`POST /jsonrpc` is a generic [JSON-RPC 2.0](https://www.jsonrpc.org/specification)
endpoint for gateways that route, validate, rate limit or log JSON-RPC traffic
(the transport style of MCP and A2A) without the protocol-specific semantics.

| Method | Params | Result |
|---|---|---|
| `echo` | anything | the params |
| `add` | `[n, ...]` or `{"a", "b"}` | the sum |
| `subtract` | `[minuend, subtrahend]` or `{"minuend", "subtrahend"}` | the difference |
| `sleep` | `[ms]` or `{"ms"}` | waits (capped by the instance delay limit: 30 s, 10 s in public mode), then answers |
| `error` | `[code, message?, data?]` or `{"code", "message", "data"}` | replies with that error object |

Behaviour per the specification:

- **Batches** (arrays) run concurrently; at most 100 entries (20 in public mode).
- **Notifications** (no `id`) get no reply; a single notification, or a batch of
  only notifications, answers `204 No Content`.
- **Errors**: `-32700` parse error (with `id: null`), `-32600` invalid request,
  `-32601` method not found (the `data` lists the available methods), `-32602`
  invalid params, `-32603` internal error.
- The HTTP status is `200` for every reply, as JSON-RPC over HTTP expects.

```hurl
{{#include ../../examples/protocols/jsonrpc.hurl:single}}
```

```hurl
{{#include ../../examples/protocols/jsonrpc.hurl:batch}}
```

```hurl
{{#include ../../examples/protocols/jsonrpc.hurl:notification}}
```

```hurl
{{#include ../../examples/protocols/jsonrpc.hurl:requested_error}}
```

```hurl
{{#include ../../examples/protocols/jsonrpc.hurl:sleep}}
```

```hurl
{{#include ../../examples/protocols/jsonrpc.hurl:parse_error}}
```
